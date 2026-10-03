//! The sheet stores parallel arrays in display order, with each package's legs
//! contiguous after it at depth one. `children` scans the following rows' parents;
//! structural edits rebuild parent indices in one pass.
//!
//! Row edits go through [`Sheet::apply`]. Result delivery, repricing ticks, and package
//! folding update pricing state without changing row identity or requests. The one
//! request change outside `apply` is `Sheet::fill_currency`, which gives a blank line
//! a default payout currency without an undo step. Sheet name, view, and refresh policy
//! are independent metadata.

use crate::core::shorthand::{identify, render_line, render_package};
use crate::core::template::{Template, TemplateSet};
use chrono::{DateTime, Utc};
use geode_core::pricing::{
    Currency, Instrument, MarketOverrides, PriceRequest, PriceResult, Shifts,
};
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

/// The status of a line with no payout currency, which has no request.
pub const NEEDS_CURRENCY: &str = "needs currency";

/// Per-sheet identity, allocated monotonically and never reused for a new row. Pricing
/// requests carry the `u64` value; document storage encodes it on an `i64` line axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Line,
    Package {
        template: Template,
    },
    /// Reserved row kind. Normal edits do not construct it, and document loading
    /// refuses it.
    Underlying,
}

/// A line's own shifts; `None` inherits the sheet's.
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

/// Periodic repricing policy: inherit the application interval, disable it, or use a
/// sheet-specific interval. Storage preserves the distinction between inherited and
/// explicit settings.
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
    /// The payout currency the line prices in; `None` until one is set,
    /// and a line without one has no request.
    pub currency: Option<Currency>,
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

/// A row record for removal/restore and document loading, including identity and
/// pricing state. Undo can restore a removed row's result without repricing. The live
/// sheet stores each field in a parallel array.
#[derive(Debug, Clone, PartialEq)]
pub struct RowRecord {
    pub id: LineId,
    pub kind: RowKind,
    /// The parent's id (not index — indices move).
    pub parent: Option<LineId>,
    pub instrument: Option<Instrument>,
    pub qty: i64,
    pub shift: OwnShifts,
    /// `None` on a package and on a line not yet given one.
    pub currency: Option<Currency>,
    pub revision: u64,
    pub result: Option<PriceResult>,
    pub state: LineState,
    pub priced_at: Option<DateTime<Utc>>,
}

/// Where an insert lands.
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
    /// request. Dropped.
    OldRevision {
        current: u64,
    },
    /// A bug: dropped and, by the tile, logged.
    FutureRevision {
        current: u64,
    },
}

/// What [`Sheet::fold_legs`] computes for a set of legs: the pricing
/// state a package row shows over them.
#[derive(Debug, Clone, PartialEq)]
pub struct Folded {
    pub result: Option<PriceResult>,
    pub state: LineState,
    pub priced_at: Option<DateTime<Utc>>,
}

#[derive(Debug)]
pub struct Sheet {
    pub name: String,
    pub view: String,
    /// Sheet-wide shifts, initialized by document loading. Live changes use
    /// `Edit::SetSheetShift` so inheriting lines receive new revisions when their
    /// effective requests change.
    pub(crate) sheet_shift: OwnShifts,
    /// Absolute spot overrides keyed by underlying. Document loading initializes them;
    /// live changes use `Edit::SetSpotOverride` to stale affected lines even though
    /// overrides travel separately from each line's request.
    pub(crate) overrides: MarketOverrides,
    pub refresh: Refresh,
    // per row, in sheet order
    ids: Vec<LineId>,
    kind: Vec<RowKind>,
    parent: Vec<Option<u32>>,
    instrument: Vec<Option<Instrument>>,
    qty: Vec<i64>,
    shift: Vec<OwnShifts>,
    currency: Vec<Option<Currency>>,
    revision: Vec<u64>,
    result: Vec<Option<PriceResult>>,
    state: Vec<LineState>,
    priced_at: Vec<Option<DateTime<Utc>>>,
    next_id: u64,
    /// The template tables `shorthand` prints against; not persisted. The
    /// builtin set until the tile sets the configured one.
    templates: Arc<TemplateSet>,
}

/// One parsed builtin set per thread, shared by every new sheet on it.
fn builtin_templates() -> Arc<TemplateSet> {
    thread_local! {
        static BUILTIN: Arc<TemplateSet> = Arc::new(TemplateSet::builtin());
    }
    BUILTIN.with(Arc::clone)
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
            currency: Vec::new(),
            revision: Vec::new(),
            result: Vec::new(),
            state: Vec::new(),
            priced_at: Vec::new(),
            next_id: 1,
            templates: builtin_templates(),
        }
    }

    pub fn set_templates(&mut self, templates: Arc<TemplateSet>) {
        self.templates = templates;
    }

    pub fn templates(&self) -> &Arc<TemplateSet> {
        &self.templates
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
    /// several underlyings (or none), or a row with no instrument; the
    /// tile's dimension context names one `underlying_ref` or nothing.
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

    /// The line's payout currency; `None` on a package and on a line
    /// that needs one.
    pub fn currency(&self, row: usize) -> Option<Currency> {
        self.currency[row]
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

    /// [`Sheet::result`] as a line's cells, sort keys and totals read it.
    /// A line whose result was priced in a currency other than its payout
    /// currency (an answer kept across a currency edit, or a currency
    /// since cleared) reads [`Currency::MIXED`], so its local figures are
    /// a gap rather than old-currency numbers under the new code; the
    /// `_usd` twins are USD either way and still read. A package's result
    /// is its fold, which [`Sheet::fold_legs`] already marks.
    pub fn shown_result(&self, row: usize) -> Option<PriceResult> {
        let mut r = self.result[row]?;
        if self.is_line(row) && self.currency[row] != Some(r.currency) {
            r.currency = Currency::MIXED;
        }
        Some(r)
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

    /// `own.or(sheet)` per field, `0.0` when both are `None`.
    pub fn effective_shifts(&self, row: usize) -> Shifts {
        let own = self.shift[row];
        Shifts {
            spot_pct: own.spot_pct.or(self.sheet_shift.spot_pct).unwrap_or(0.0),
            vol_pts: own.vol_pts.or(self.sheet_shift.vol_pts).unwrap_or(0.0),
        }
    }

    /// The one place a line's request is assembled. `None` on a package
    /// and on a line without a payout currency: a request never guesses
    /// the currency a figure is reported in.
    pub fn request(&self, row: usize) -> Option<PriceRequest> {
        let currency = self.currency[row]?;
        self.instrument[row]
            .as_ref()
            .map(|instrument| PriceRequest {
                instrument: instrument.clone(),
                shifts: self.effective_shifts(row),
                currency,
            })
    }

    /// Lines (never packages) that are `Stale` and have a request: what
    /// the tile submits. A line without a currency stays `Stale` but is
    /// not counted as pricing, since nothing can be asked for it.
    pub fn stale_lines(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.len()).filter(|r| {
            self.is_line(*r) && self.state[*r] == LineState::Stale && self.currency[*r].is_some()
        })
    }

    /// Lines (never packages) whose payout currency is blank.
    pub fn lines_needing_currency(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.len()).filter(|r| self.is_line(*r) && self.currency[*r].is_none())
    }

    pub fn record(&self, row: usize) -> RowRecord {
        RowRecord {
            id: self.ids[row],
            kind: self.kind[row],
            parent: self.parent(row).map(|p| self.ids[p]),
            instrument: self.instrument[row].clone(),
            qty: self.qty[row],
            shift: self.shift[row],
            currency: self.currency[row],
            revision: self.revision[row],
            result: self.result[row],
            state: self.state[row].clone(),
            priced_at: self.priced_at[row],
        }
    }

    /// Install a result only when its line exists at the answered revision, then fold
    /// packages. A failure sets `Failed`, retains the previous result, and records the
    /// attempt time; result cells display a dash while failed. Use
    /// [`Sheet::deliver_all`] for a batch so packages fold once.
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

    /// Install a pricing batch and fold packages once after all answers. Return one
    /// [`Delivered`] outcome per answer, in input order. Folding after each answer
    /// would repeatedly scan the sheet.
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

    /// Mark every line stale at its current revision and fold packages. Refresh ticks
    /// and `:price` do not edit requests, so a result already in flight at the current
    /// revision remains eligible to install. Failed lines become eligible for retry
    /// too.
    pub fn mark_all_stale(&mut self) {
        for row in 0..self.len() {
            if self.is_line(row) {
                self.state[row] = LineState::Stale;
            }
        }
        self.fold_packages();
    }

    /// One result into its row; everything `deliver` does except the
    /// fold, so a batch can fold once.
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
            // At a matching revision the line's currency is the one it was
            // requested in. A pricer answering in another must not install
            // figures in units the row does not claim: it fails like a
            // refusal, keeping the last good result.
            Ok(r) => match self.currency[row] {
                Some(want) if r.currency != want => {
                    self.state[row] = LineState::Failed(format!(
                        "priced in {}, asked for {}",
                        r.currency.as_str(),
                        want.as_str()
                    ));
                }
                _ => {
                    self.result[row] = Some(r);
                    self.state[row] = LineState::Fresh;
                }
            },
            Err(message) => self.state[row] = LineState::Failed(message),
        }
        self.priced_at[row] = Some(at);
        Delivered::Installed
    }

    /// Fold every package's result from all of its legs with
    /// [`Sheet::fold_legs`].
    pub fn fold_packages(&mut self) {
        for p in 0..self.len() {
            if !self.is_package(p) {
                continue;
            }
            let Folded {
                result,
                state,
                priced_at,
            } = self.fold_legs(self.children(p));
            self.result[p] = result;
            self.state[p] = state;
            self.priced_at[p] = priced_at;
        }
    }

    /// Fold `legs` (lines of one package, in sheet order) as
    /// `Σ qty_leg × value_leg`: the one fold a package row, a package
    /// showing only some of its legs, and a selection total share, so a
    /// sum over a subset of legs is computed exactly as the full one.
    ///
    /// Failure takes precedence over staleness and names the first failed
    /// leg, a leg without a currency counting as failed with
    /// [`NEEDS_CURRENCY`]; otherwise any stale leg makes the fold stale. A result exists
    /// only for a nonempty set whose legs all have results and none has
    /// failed. Its currency is the legs' when they agree, each in its own
    /// payout currency, and [`Currency::MIXED`] otherwise: the local arrays are then sums
    /// of unlike units, and a local cell or total over them paints a gap
    /// rather than a plausible number. `priced_at` is the oldest present
    /// leg timestamp, including failed attempts.
    pub fn fold_legs(&self, legs: impl IntoIterator<Item = usize>) -> Folded {
        // An empty set has no sum.
        let mut any = false;
        let mut complete = true;
        let mut sum: Option<PriceResult> = None;
        let mut stale = false;
        let mut failed: Option<String> = None;
        let mut oldest: Option<DateTime<Utc>> = None;
        for leg in legs {
            any = true;
            match &self.state[leg] {
                // A leg without a currency is never requested, so its
                // `Stale` would leave the package reading `pricing…` for
                // good: it fails the fold, naming the leg, instead.
                _ if self.currency[leg].is_none() => {
                    if failed.is_none() {
                        failed = Some(format!(
                            "{}: {NEEDS_CURRENCY}",
                            render_line(
                                self.qty[leg],
                                self.instrument[leg].as_ref().expect("a leg is a line")
                            )
                        ));
                    }
                }
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
                    let acc = sum.get_or_insert_with(|| PriceResult::zero(r.currency));
                    acc.add_scaled(q, &r);
                    // The first leg names the currency; a leg in another
                    // makes the local sum one of unlike units. Once mixed
                    // it stays mixed: no leg's currency equals the marker.
                    // A leg holding an answer in a currency it no longer
                    // asks for mixes it too, or a package whose legs all
                    // moved would paint the old sum under the new code.
                    if acc.currency != r.currency || self.currency[leg] != Some(r.currency) {
                        acc.currency = Currency::MIXED;
                    }
                }
                None => complete = false,
            }
            oldest = match (oldest, self.priced_at[leg]) {
                (None, t) => t,
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
            };
        }
        // An empty set has no sum (`any` is false), so `sum` is `None`
        // exactly when the result must be.
        Folded {
            result: if any && complete && failed.is_none() {
                sum
            } else {
                None
            },
            state: match failed {
                Some(m) => LineState::Failed(m),
                None if stale => LineState::Stale,
                None => LineState::Fresh,
            },
            priced_at: oldest,
        }
    }

    /// The row in the grammar: a line; a package in template
    /// form while its legs match the table, else its legs one per line;
    /// an empty package as nothing.
    pub fn shorthand(&self, row: usize) -> String {
        match self.kind[row] {
            RowKind::Line | RowKind::Underlying => match &self.instrument[row] {
                Some(i) => render_line(self.qty[row], i),
                None => String::new(),
            },
            RowKind::Package { template } => {
                let legs = self.package_legs(row);
                self.templates
                    .resolve(template.token())
                    .and_then(|def| render_package(def, &legs))
                    .unwrap_or_else(|| {
                        legs.iter()
                            .map(|(q, i)| render_line(*q, i))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
            }
        }
    }

    /// A package's legs as the shorthand reads them: (qty, instrument)
    /// in sheet order.
    fn package_legs(&self, row: usize) -> Vec<(i64, &Instrument)> {
        self.children(row)
            .filter_map(|l| self.instrument[l].as_ref().map(|i| (self.qty[l], i)))
            .collect()
    }

    /// The template package `row`'s legs now form ([`identify`]);
    /// `None` on a row that is not a package.
    pub(crate) fn identified_template(&self, row: usize) -> Option<Template> {
        let RowKind::Package { template } = self.kind[row] else {
            return None;
        };
        Some(identify(&self.templates, template, &self.package_legs(row)))
    }

    // ---- the structural primitives `edit.rs` builds on (pub(crate)) ----

    /// The next fresh id; never reused.
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
        self.currency.insert(at, rec.currency);
        self.revision.insert(at, rec.revision);
        self.result.insert(at, rec.result);
        self.state.insert(at, rec.state);
        self.priced_at.insert(at, rec.priced_at);
        self.next_id = self.next_id.max(rec.id.0 + 1);
    }

    /// Remove one row at flat `at`. Callers needing undo records must capture the
    /// entire removal range first: stored parent indices shift as rows are removed, so
    /// a record captured partway through removal could name the wrong parent.
    pub(crate) fn take_out(&mut self, at: usize) {
        self.ids.remove(at);
        self.kind.remove(at);
        self.parent.remove(at);
        self.instrument.remove(at);
        self.qty.remove(at);
        self.shift.remove(at);
        self.currency.remove(at);
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
                // Packages are always roots; only lines can be legs.
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

    /// `row` must be a package.
    pub(crate) fn set_template(&mut self, row: usize, template: Template) {
        debug_assert!(self.is_package(row));
        self.kind[row] = RowKind::Package { template };
    }

    pub(crate) fn set_qty(&mut self, row: usize, qty: i64) {
        self.qty[row] = qty;
    }

    pub(crate) fn set_shift(&mut self, row: usize, shift: OwnShifts) {
        self.shift[row] = shift;
    }

    pub(crate) fn set_currency(&mut self, row: usize, currency: Option<Currency>) {
        self.currency[row] = currency;
    }

    /// Give a blank line `currency` and reprice it; false, changing
    /// nothing, on a package or a line that already has one, so a fill
    /// never overwrites a currency the user chose. Not an undo step: the
    /// fill supplies a default, and undoing past it would only leave the
    /// line blank for the next fill.
    pub(crate) fn fill_currency(&mut self, row: usize, currency: Currency) -> bool {
        if !self.is_line(row) || self.currency[row].is_some() {
            return false;
        }
        self.currency[row] = Some(currency);
        self.touch(row);
        self.fold_packages();
        true
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
        self.currency[whole.clone()].rotate_left(by);
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
            currency: spec.currency,
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
            currency: None,
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
    use geode_core::pricing::{Expiry, Measure, OptionKind, Strike, Vanilla};

    pub(crate) fn spx(strike: f64, kind: OptionKind) -> Instrument {
        Instrument::Vanilla(Vanilla {
            underlying: "SPX".into(),
            expiry: Expiry::Date(chrono::NaiveDate::from_ymd_opt(2026, 12, 18).unwrap()),
            strike: Strike::Absolute(strike),
            kind,
        })
    }

    /// A line in USD, as most fixtures price.
    pub(crate) fn line(instrument: Instrument, qty: i64) -> RowSpec {
        line_in(instrument, qty, Some(Currency::USD))
    }

    pub(crate) fn line_in(instrument: Instrument, qty: i64, currency: Option<Currency>) -> RowSpec {
        RowSpec::Line(LineSpec {
            instrument,
            qty,
            shift: OwnShifts::default(),
            currency,
        })
    }

    /// `spec` with every line in USD: the shorthand leaves a line's
    /// currency blank, and a blank line never prices.
    pub(crate) fn in_usd(mut spec: RowSpec) -> RowSpec {
        match &mut spec {
            RowSpec::Line(l) => l.currency = Some(Currency::USD),
            RowSpec::Package { legs, .. } => {
                for l in legs {
                    l.currency = Some(Currency::USD);
                }
            }
        }
        spec
    }

    /// A call spread whose legs price in USD.
    pub(crate) fn callspread(qty: i64) -> RowSpec {
        in_usd(
            crate::core::shorthand::parse_builtin(&format!("{qty} SPX Z26 4800/5200 CS")).unwrap(),
        )
    }

    pub(crate) fn eur() -> Currency {
        Currency::parse("EUR").unwrap()
    }

    /// Move `row` to EUR and answer it with `r` in EUR, as a pricer asked
    /// in EUR does. The move is a new request, so the answer carries the
    /// line's new revision.
    pub(crate) fn deliver_in_eur(
        s: &mut Sheet,
        row: usize,
        mut r: PriceResult,
        t: DateTime<Utc>,
    ) -> Delivered {
        s.apply(Edit::SetCurrency {
            row,
            currency: Some(eur()),
        })
        .unwrap();
        r.currency = eur();
        s.deliver(s.id(row), s.revision(row), Ok(r), t)
    }

    pub(crate) fn result(price: f64) -> PriceResult {
        let mut r = PriceResult::zero(Currency::USD);
        r.set(Measure::Npv, false, price);
        r.set(Measure::Delta01, false, price / 10.0);
        r.set(Measure::Gamma01, false, 0.01);
        r.set(Measure::Vega01, false, 1.0);
        r.set(Measure::CleanThetaBusinessDay, false, -0.5);
        r.set(Measure::Rho010, false, 0.1);
        for m in Measure::ALL {
            r.set(m, true, r.get(m, false) * 1.08);
        }
        r
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

    /// A sheet named `book` holding one CS package (rows 0, 1, 2: the
    /// package and its two legs) then one bare line (row 3), both parsed
    /// through the shorthand the package tests use.
    pub(crate) fn sheet_with_package_and_line() -> Sheet {
        let mut s = Sheet::new("book");
        push(
            &mut s,
            vec![
                in_usd(crate::core::shorthand::parse_builtin("SPX Z26 100/105 CS").unwrap()),
                in_usd(crate::core::shorthand::parse_builtin("NDX H27 95 P").unwrap()),
            ],
        );
        assert_eq!(s.len(), 4, "a package, two legs, a line");
        s
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
            template: Template::CUSTOM,
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
        // A package at a leg place is refused: packages cannot nest.
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
        // A leg alone: the package stays, possibly empty.
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
        assert_eq!(sum.get(Measure::Npv, false), -300.0);
        assert_eq!(sum.get(Measure::Delta01, false), -30.0);
        assert_eq!(sum.get(Measure::Gamma01, false), 0.0);
        assert_eq!(sum.get(Measure::Gamma02, false), 0.0);
        assert_eq!(sum.get(Measure::Vega01, false), 0.0);
        assert_eq!(sum.get(Measure::CleanThetaBusinessDay, false), 0.0);
        assert_eq!(sum.get(Measure::Rho010, false), 0.0);
        assert_eq!(sum.get(Measure::Npv, true), -324.0, "usd folds too");
        assert_eq!(s.state(0), &LineState::Fresh);
        assert_eq!(
            s.priced_at(0),
            Some(at(0)),
            "a package is as old as its oldest leg"
        );
        // Failed wins over Stale and names the leg.
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
    fn a_package_over_differing_currencies_folds_to_a_mixed_currency() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![callspread(-5)]);
        let (long, short) = (s.id(1), s.id(2));
        s.deliver(long, 1, Ok(result(100.0)), at(0));
        deliver_in_eur(&mut s, 2, result(40.0), at(1));
        let sum = s.result(0).unwrap();
        assert!(
            sum.currency.is_mixed(),
            "USD and EUR legs: {:?}",
            sum.currency
        );
        assert_eq!(sum.get(Measure::Npv, true), -324.0, "usd still folds");
        assert_eq!(s.state(0), &LineState::Fresh);
        // Repricing the EUR leg in USD makes the package USD again.
        s.apply(Edit::SetCurrency {
            row: 2,
            currency: Some(Currency::USD),
        })
        .unwrap();
        s.deliver(short, s.revision(2), Ok(result(40.0)), at(2));
        assert_eq!(s.result(0).unwrap().currency, Currency::USD);
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
        assert_eq!(s.result(0).unwrap().get(Measure::Npv, false), -300.0);
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
            s.result(0).unwrap().get(Measure::Npv, false),
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

    /// Refresh ticks and `:price` mark every line stale, including failed lines that
    /// may succeed on retry. Revisions remain unchanged so in-flight answers can
    /// install; package state follows its legs.
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

    #[test]
    fn a_line_without_a_currency_has_no_request_and_needs_currency() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![line_in(spx(5000.0, OptionKind::Call), 1, None)],
        );
        assert_eq!(s.currency(0), None);
        assert_eq!(s.request(0), None, "nothing to price in");
        assert_eq!(
            s.stale_lines().count(),
            0,
            "a line that cannot be requested is not submitted"
        );
        assert_eq!(s.lines_needing_currency().collect::<Vec<_>>(), vec![0]);
        let status = crate::core::columns::column("status").unwrap();
        let cell = crate::core::columns::cell_text(
            &s,
            0,
            status,
            &status.default_format,
            geode_core::clock::Clock::utc(),
        );
        assert_eq!(cell.text, "needs currency");
    }

    /// A blank leg is never requested: the package names it as failing
    /// rather than reading `pricing…` for good.
    #[test]
    fn a_package_with_a_blank_leg_fails_naming_it() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![crate::core::shorthand::parse_builtin("-5 SPX Z26 4800/5200 CS").unwrap()],
        );
        assert_eq!(s.lines_needing_currency().collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(
            s.state(0),
            &LineState::Failed("-5 SPX Z26 4800 C: needs currency".into())
        );
        assert!(s.fill_currency(1, Currency::USD));
        assert_eq!(
            s.state(0),
            &LineState::Failed("5 SPX Z26 5200 C: needs currency".into()),
            "the fill refolds"
        );
        assert!(s.fill_currency(2, Currency::USD));
        assert_eq!(s.state(0), &LineState::Stale, "both legs now price");
    }

    #[test]
    fn a_line_with_a_currency_requests_in_it() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![line_in(spx(5000.0, OptionKind::Call), 1, Some(eur()))],
        );
        assert_eq!(s.currency(0), Some(eur()));
        assert_eq!(s.request(0).unwrap().currency, eur());
        assert_eq!(s.lines_needing_currency().count(), 0);
    }

    #[test]
    fn a_result_in_another_currency_fails_the_line() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        let mut r = result(10.0);
        r.currency = eur();
        assert_eq!(s.deliver(s.id(0), 1, Ok(r), at(0)), Delivered::Installed);
        assert_eq!(
            s.state(0),
            &LineState::Failed("priced in EUR, asked for USD".into())
        );
        assert_eq!(s.result(0), None, "the mismatched result is not installed");
        assert_eq!(s.priced_at(0), Some(at(0)), "the attempt is recorded");
    }

    /// The revision is the request's identity: a result asked in USD that
    /// lands after the line moved to EUR answers an older request.
    #[test]
    fn a_result_after_a_currency_edit_is_discarded() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        let asked = s.revision(0);
        s.apply(Edit::SetCurrency {
            row: 0,
            currency: Some(eur()),
        })
        .unwrap();
        assert_eq!(
            s.deliver(s.id(0), asked, Ok(result(10.0)), at(0)),
            Delivered::OldRevision { current: asked + 1 }
        );
        assert_eq!(s.result(0), None);
        assert_eq!(s.state(0), &LineState::Stale);
    }

    #[test]
    fn undoing_a_remove_restores_the_currency() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![line_in(spx(5000.0, OptionKind::Call), 1, None)],
        );
        assert!(s.fill_currency(0, eur()));
        let undo = s.apply(Edit::Remove { at: 0 }).unwrap();
        s.undo(&undo).unwrap();
        assert_eq!(s.currency(0), Some(eur()));
        assert_eq!(s.request(0).unwrap().currency, eur());
    }

    #[test]
    fn fill_currency_touches_only_blank_lines() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![
                line_in(spx(5000.0, OptionKind::Call), 1, None),
                line(spx(5100.0, OptionKind::Call), 1),
                callspread(1),
            ],
        );
        let before = s.revision(0);
        assert!(s.fill_currency(0, eur()), "a blank line fills");
        assert_eq!(s.currency(0), Some(eur()));
        assert_eq!(s.revision(0), before + 1, "the fill reprices the line");
        assert_eq!(s.state(0), &LineState::Stale);
        assert!(
            !s.fill_currency(0, Currency::USD),
            "a set line keeps its own"
        );
        assert_eq!(s.currency(0), Some(eur()));
        let usd_rev = s.revision(1);
        assert!(!s.fill_currency(1, eur()));
        assert_eq!(
            (s.currency(1), s.revision(1)),
            (Some(Currency::USD), usd_rev)
        );
        assert!(!s.fill_currency(2, eur()), "a package carries no currency");
        assert_eq!(s.currency(2), None);
    }
}
