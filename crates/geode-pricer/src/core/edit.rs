//! Row edits and their inverses. [`Sheet::apply`] compares affected line requests
//! before and after an edit, increments changed revisions, and folds packages. Spot
//! overrides are carried separately in pricing parameters and explicitly stale their
//! affected lines. Restoring removed rows preserves their identities and pricing state.

use crate::core::sheet::{LineId, LineSpec, OwnShifts, Place, RowKind, RowRecord, RowSpec, Sheet};
use crate::core::template::Template;
use geode_core::pricing::{Instrument, PriceRequest};
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    /// A line, or a package with its legs, or several roots.
    Insert {
        place: Place,
        rows: Vec<RowSpec>,
    },
    /// A package removes its legs.
    Remove {
        at: usize,
    },
    /// Restore row records with their identities and pricing state. Used by undo and by
    /// document loading, which validates and seeds records before applying them.
    Restore {
        at: usize,
        rows: Vec<RowRecord>,
    },
    SetInstrument {
        row: usize,
        instrument: Instrument,
    },
    SetQty {
        row: usize,
        qty: i64,
    },
    SetShift {
        row: usize,
        shift: OwnShifts,
    },
    /// Within the parent; `delta` in sibling steps.
    Move {
        row: usize,
        delta: isize,
    },
    /// `first` and the next `count − 1` roots (all lines) become one
    /// package; `id: None` takes a fresh id, `Some` is undo's.
    Group {
        first: usize,
        count: usize,
        template: Template,
        id: Option<LineId>,
    },
    Ungroup {
        row: usize,
    },
    /// Name a package without reading its legs: undo's, restoring the
    /// name a leg edit replaced.
    SetTemplate {
        row: usize,
        template: Template,
    },
    SetSheetShift(OwnShifts),
    /// `None` clears.
    SetSpotOverride {
        underlying: String,
        level: Option<f64>,
    },
}

/// The package an edit may reshape, so `apply` can rename it after.
enum Reshaped {
    None,
    /// A leg changed, arrived, left or moved; the package by id, since
    /// the edit may move its row.
    Legs(LineId),
    /// A fresh `Group`'s package, at its `first` row.
    Grouped(usize),
}

/// The inverse of one `apply`, in the order to apply it.
#[derive(Debug, Clone, PartialEq)]
pub struct Undo {
    pub inverse: Vec<Edit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    NoSuchRow(usize),
    NotAPackage(usize),
    NotALine(usize),
    /// A `Place::Root { at }` inside a package's leg run.
    NotARootBoundary(usize),
    /// A package cannot be inserted as another package's leg.
    PackageInsidePackage,
    LegOutOfRange {
        package: usize,
        leg: usize,
    },
    IdInUse(LineId),
    NoSuchParent(LineId),
    ZeroQty,
    EmptyInsert,
    NotContiguousRoots,
    MoveOffEnd,
}

impl fmt::Display for EditError {
    /// The footer's text.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditError::NoSuchRow(r) => write!(f, "no row {r}"),
            EditError::NotAPackage(r) => write!(f, "row {r} is not a package"),
            EditError::NotALine(r) => write!(f, "row {r} is not a line"),
            EditError::NotARootBoundary(r) => write!(f, "row {r} is inside a package"),
            EditError::PackageInsidePackage => write!(f, "a package cannot hold a package"),
            EditError::LegOutOfRange { package, leg } => {
                write!(f, "package {package} has no leg slot {leg}")
            }
            EditError::IdInUse(id) => write!(f, "line {} already exists", id.0),
            EditError::NoSuchParent(id) => write!(f, "no package {} to restore into", id.0),
            EditError::ZeroQty => write!(f, "quantity must not be zero"),
            EditError::EmptyInsert => write!(f, "nothing to insert"),
            EditError::NotContiguousRoots => {
                write!(f, "group needs a contiguous run of top-level lines")
            }
            EditError::MoveOffEnd => write!(f, "cannot move past the end"),
        }
    }
}

impl Sheet {
    /// Apply a row edit and return its inverse. Increment the revision and mark stale
    /// each line whose request changed; spot override edits explicitly touch matching
    /// lines. Fold packages after success. A refused edit leaves the sheet unchanged.
    pub fn apply(&mut self, edit: Edit) -> Result<Undo, EditError> {
        let touched = self.touched_by(&edit);
        let before: Vec<(LineId, Option<PriceRequest>)> = touched
            .iter()
            .map(|id| (*id, self.index_of(*id).and_then(|r| self.request(r))))
            .collect();
        let reshaped = self.reshaped_by(&edit);
        let mut undo = self.apply_inner(edit)?;
        self.reidentify(reshaped, &mut undo);
        for (id, old) in before {
            if let Some(row) = self.index_of(id)
                && self.request(row) != old
            {
                self.touch(row);
            }
        }
        self.fold_packages();
        Ok(undo)
    }

    /// IDs whose requests may change, retained across row movement for the before/after
    /// comparison. `SetSpotOverride` carries data in batch parameters, so `apply_inner`
    /// touches matching lines explicitly.
    fn touched_by(&self, edit: &Edit) -> Vec<LineId> {
        match edit {
            Edit::SetInstrument { row, .. } | Edit::SetShift { row, .. } => (*row < self.len())
                .then(|| self.id(*row))
                .into_iter()
                .collect(),
            Edit::SetSheetShift(_) => (0..self.len())
                .filter(|r| self.is_line(*r))
                .map(|r| self.id(r))
                .collect(),
            Edit::Insert { .. }
            | Edit::Remove { .. }
            | Edit::Restore { .. }
            | Edit::SetQty { .. }
            | Edit::Move { .. }
            | Edit::Group { .. }
            | Edit::Ungroup { .. }
            | Edit::SetTemplate { .. }
            | Edit::SetSpotOverride { .. } => Vec::new(),
        }
    }

    /// The package whose leg set `edit` may reshape. Restore, root inserts
    /// and `SetTemplate` reshape nothing: a loaded or typed package keeps
    /// the name it carries.
    fn reshaped_by(&self, edit: &Edit) -> Reshaped {
        let parent_of = |row: usize| {
            (row < self.len())
                .then(|| self.parent(row))
                .flatten()
                .map_or(Reshaped::None, |p| Reshaped::Legs(self.id(p)))
        };
        match edit {
            Edit::SetInstrument { row, .. } | Edit::SetQty { row, .. } | Edit::Move { row, .. } => {
                parent_of(*row)
            }
            Edit::Remove { at } => parent_of(*at),
            Edit::Insert {
                place: Place::Leg { package, .. },
                ..
            } if *package < self.len() => Reshaped::Legs(self.id(*package)),
            Edit::Group {
                first, id: None, ..
            } => Reshaped::Grouped(*first),
            _ => Reshaped::None,
        }
    }

    /// Rename a reshaped package to the template its legs now form. A leg
    /// edit's undo restores the replaced name after its own inverse, which
    /// re-identifies first: that keeps a name no table fits any more (a
    /// reload redefined it) through an undo. A fresh group's undo is its
    /// `Ungroup`, which needs no name.
    fn reidentify(&mut self, reshaped: Reshaped, undo: &mut Undo) {
        let (row, record) = match reshaped {
            Reshaped::None => return,
            Reshaped::Legs(id) => match self.index_of(id) {
                Some(row) => (row, true),
                None => return,
            },
            Reshaped::Grouped(row) => (row, false),
        };
        let (RowKind::Package { template: old }, Some(new)) =
            (self.kind(row), self.identified_template(row))
        else {
            return;
        };
        if new != old {
            self.set_template(row, new);
            if record {
                undo.inverse.push(Edit::SetTemplate { row, template: old });
            }
        }
    }

    fn apply_inner(&mut self, edit: Edit) -> Result<Undo, EditError> {
        match edit {
            Edit::Insert { place, rows } => self.insert(place, rows),
            Edit::Remove { at } => self.remove(at),
            Edit::Restore { at, rows } => self.restore(at, rows),
            Edit::SetInstrument { row, instrument } => {
                self.row_exists(row)?;
                if !self.is_line(row) {
                    return Err(EditError::NotALine(row));
                }
                let old = self
                    .instrument(row)
                    .cloned()
                    .expect("a line has an instrument");
                self.set_instrument(row, instrument);
                Ok(Undo {
                    inverse: vec![Edit::SetInstrument {
                        row,
                        instrument: old,
                    }],
                })
            }
            Edit::SetQty { row, qty } => {
                self.row_exists(row)?;
                if !self.is_line(row) {
                    return Err(EditError::NotALine(row));
                }
                if qty == 0 {
                    return Err(EditError::ZeroQty);
                }
                let old = self.qty(row);
                self.set_qty(row, qty);
                Ok(Undo {
                    inverse: vec![Edit::SetQty { row, qty: old }],
                })
            }
            Edit::SetShift { row, shift } => {
                self.row_exists(row)?;
                if !self.is_line(row) {
                    return Err(EditError::NotALine(row));
                }
                let old = self.shift(row);
                self.set_shift(row, shift);
                Ok(Undo {
                    inverse: vec![Edit::SetShift { row, shift: old }],
                })
            }
            Edit::Move { row, delta } => self.move_row(row, delta),
            Edit::Group {
                first,
                count,
                template,
                id,
            } => self.group(first, count, template, id),
            Edit::Ungroup { row } => self.ungroup(row),
            Edit::SetTemplate { row, template } => {
                self.row_exists(row)?;
                let RowKind::Package { template: old } = self.kind(row) else {
                    return Err(EditError::NotAPackage(row));
                };
                self.set_template(row, template);
                Ok(Undo {
                    inverse: vec![Edit::SetTemplate { row, template: old }],
                })
            }
            Edit::SetSheetShift(shift) => {
                let old = self.sheet_shift;
                self.sheet_shift = shift;
                Ok(Undo {
                    inverse: vec![Edit::SetSheetShift(old)],
                })
            }
            Edit::SetSpotOverride { underlying, level } => {
                let key = underlying.to_ascii_uppercase();
                let old = self.overrides.spot.get(&key).copied();
                if old != level {
                    match level {
                        Some(l) => {
                            self.overrides.spot.insert(key.clone(), l);
                        }
                        None => {
                            self.overrides.spot.remove(&key);
                        }
                    }
                    // Overrides travel in `PriceParams`, outside each line's request.
                    // Touch matching lines explicitly so revision checks reject answers
                    // priced with an old override.
                    for row in 0..self.len() {
                        if self.is_line(row)
                            && self.instrument(row).is_some_and(|i| i.underlying() == key)
                        {
                            self.touch(row);
                        }
                    }
                }
                Ok(Undo {
                    inverse: vec![Edit::SetSpotOverride {
                        underlying: key,
                        level: old,
                    }],
                })
            }
        }
    }

    fn row_exists(&self, row: usize) -> Result<(), EditError> {
        if row < self.len() {
            Ok(())
        } else {
            Err(EditError::NoSuchRow(row))
        }
    }

    fn insert(&mut self, place: Place, rows: Vec<RowSpec>) -> Result<Undo, EditError> {
        if rows.is_empty() {
            return Err(EditError::EmptyInsert);
        }
        let zero = rows.iter().any(|r| match r {
            RowSpec::Line(l) => l.qty == 0,
            RowSpec::Package { legs, .. } => legs.iter().any(|l| l.qty == 0),
        });
        if zero {
            return Err(EditError::ZeroQty);
        }
        match place {
            Place::Root { at } => {
                if at > self.len() {
                    return Err(EditError::NoSuchRow(at));
                }
                if at < self.len() && self.depth(at) != 0 {
                    return Err(EditError::NotARootBoundary(at));
                }
                let mut cursor = at;
                for spec in &rows {
                    match spec {
                        RowSpec::Line(l) => {
                            let rec = self.new_record(l, None);
                            self.splice_in(cursor, rec, None);
                            cursor += 1;
                        }
                        RowSpec::Package { template, legs } => {
                            let pkg = self.new_package_record(*template, None);
                            let pkg_id = pkg.id;
                            let pkg_row = cursor;
                            self.splice_in(cursor, pkg, None);
                            cursor += 1;
                            for l in legs {
                                let rec = self.new_record(l, Some(pkg_id));
                                self.splice_in(cursor, rec, Some(pkg_row));
                                cursor += 1;
                            }
                        }
                    }
                }
                self.reindex_parents();
                Ok(Undo {
                    inverse: vec![Edit::Remove { at }; rows.len()],
                })
            }
            Place::Leg { package, leg } => {
                self.row_exists(package)?;
                if !self.is_package(package) {
                    return Err(EditError::NotAPackage(package));
                }
                let children = self.children(package);
                if leg > children.len() {
                    return Err(EditError::LegOutOfRange { package, leg });
                }
                let mut lines: Vec<&LineSpec> = Vec::with_capacity(rows.len());
                for spec in &rows {
                    match spec {
                        RowSpec::Line(l) => lines.push(l),
                        RowSpec::Package { .. } => return Err(EditError::PackageInsidePackage),
                    }
                }
                let at = children.start + leg;
                let pkg_id = self.id(package);
                for (cursor, l) in (at..).zip(lines) {
                    let rec = self.new_record(l, Some(pkg_id));
                    self.splice_in(cursor, rec, Some(package));
                }
                self.reindex_parents();
                Ok(Undo {
                    inverse: vec![Edit::Remove { at }; rows.len()],
                })
            }
        }
    }

    fn remove(&mut self, at: usize) -> Result<Undo, EditError> {
        self.row_exists(at)?;
        let count = 1 + self.children(at).len();
        // Capture every record in the range BEFORE removing any of them:
        // `parent` stores flat indices, so once the first row is gone the
        // remaining ones' stored index no longer points at the removed
        // package — a record taken between `take_out` calls would
        // self-reference instead (see `take_out`'s own doc comment).
        let rows: Vec<RowRecord> = (at..at + count).map(|r| self.record(r)).collect();
        for _ in 0..count {
            self.take_out(at);
        }
        self.reindex_parents();
        Ok(Undo {
            inverse: vec![Edit::Restore { at, rows }],
        })
    }

    /// Restore trusts each record's structure, state, and revision. Undo supplies
    /// records from a strictly LIFO history; document loading validates records and
    /// initializes them stale. Replaying saved results after an unrelated request
    /// change could incorrectly mark an old price fresh.
    fn restore(&mut self, at: usize, rows: Vec<RowRecord>) -> Result<Undo, EditError> {
        if rows.is_empty() {
            return Err(EditError::EmptyInsert);
        }
        if at > self.len() {
            return Err(EditError::NoSuchRow(at));
        }
        if let Some(rec) = rows.iter().find(|r| self.has_id(r.id)) {
            return Err(EditError::IdInUse(rec.id));
        }
        // Refuse missing parent IDs. Callers supply valid package/leg ordering;
        // `reindex_parents` derives the actual parent from that order.
        for rec in &rows {
            if let Some(pid) = rec.parent
                && !self.has_id(pid)
                && !rows
                    .iter()
                    .any(|r| r.id == pid && matches!(r.kind, RowKind::Package { .. }))
            {
                return Err(EditError::NoSuchParent(pid));
            }
        }
        let n = rows.len();
        for (cursor, rec) in (at..).zip(rows) {
            let leg = rec.parent.is_some();
            self.splice_in(cursor, rec, None);
            self.set_leg_marker(cursor, leg);
        }
        self.reindex_parents();
        // The inverse removes what was restored: each `Remove { at }`
        // takes one ROOT (with its legs) or one leg. Count the top-level
        // records restored: roots, plus legs whose parent was NOT in the
        // batch.
        let restored_top = (at..at + n)
            .filter(|r| self.parent(*r).is_none_or(|p| p < at))
            .count();
        Ok(Undo {
            inverse: vec![Edit::Remove { at }; restored_top],
        })
    }

    /// Apply `undo`'s edits in order; the redo is their inverses in
    /// reverse (the last one applied is the first to take back). NOT
    /// atomic: an inverse refused partway through leaves every earlier
    /// inverse in `undo.inverse` already applied and answers that error —
    /// there is no dry-run validation, and a redo is answered only on
    /// full success.
    pub fn undo(&mut self, undo: &Undo) -> Result<Undo, EditError> {
        let mut inverses = Vec::with_capacity(undo.inverse.len());
        for edit in &undo.inverse {
            inverses.extend(self.apply(edit.clone())?.inverse);
        }
        inverses.reverse();
        Ok(Undo { inverse: inverses })
    }

    /// The flat block a row occupies: itself plus its legs.
    pub(crate) fn block(&self, row: usize) -> std::ops::Range<usize> {
        row..self.children(row).end
    }

    /// The siblings of `row`, in order: the roots, or the legs of its package.
    pub(crate) fn siblings(&self, row: usize) -> Vec<usize> {
        match self.parent(row) {
            None => self.roots().collect(),
            Some(p) => self.children(p).collect(),
        }
    }

    fn move_row(&mut self, row: usize, delta: isize) -> Result<Undo, EditError> {
        self.row_exists(row)?;
        let siblings = self.siblings(row);
        let pos = siblings
            .iter()
            .position(|s| *s == row)
            .expect("a row is among its siblings");
        let target = pos as isize + delta;
        if target < 0 || target as usize >= siblings.len() {
            return Err(EditError::MoveOffEnd);
        }
        let id = self.id(row);
        let mut current = row;
        for _ in 0..delta.unsigned_abs() {
            let sibs = self.siblings(current);
            let p = sibs
                .iter()
                .position(|s| *s == current)
                .expect("still a sibling");
            if delta > 0 {
                let next = sibs[p + 1];
                let (a, b) = (self.block(current), self.block(next));
                self.swap_adjacent_blocks(a.clone(), b.clone());
                current = a.start + b.len();
            } else {
                let prev = sibs[p - 1];
                let (a, b) = (self.block(prev), self.block(current));
                self.swap_adjacent_blocks(a.clone(), b);
                current = a.start;
            }
            self.reindex_parents();
        }
        debug_assert_eq!(self.id(current), id);
        Ok(Undo {
            inverse: vec![Edit::Move {
                row: current,
                delta: -delta,
            }],
        })
    }

    fn group(
        &mut self,
        first: usize,
        count: usize,
        template: Template,
        id: Option<LineId>,
    ) -> Result<Undo, EditError> {
        self.row_exists(first)?;
        if count == 0 {
            return Err(EditError::EmptyInsert);
        }
        if let Some(id) = id
            && self.has_id(id)
        {
            return Err(EditError::IdInUse(id));
        }
        // `first` and the next `count − 1` rows must each be a root line:
        // a root line has no legs, so consecutive root lines are
        // consecutive rows.
        // A typed count can be any usize; saturating keeps an absurd one a
        // refusal rather than an overflow.
        let end = first.saturating_add(count);
        if end > self.len() {
            return Err(EditError::NotContiguousRoots);
        }
        if (first..end).any(|r| self.depth(r) != 0 || !self.is_line(r)) {
            return Err(EditError::NotContiguousRoots);
        }
        let pkg = self.new_package_record(template, id);
        self.splice_in(first, pkg, None);
        for r in first + 1..=end {
            self.set_leg_marker(r, true);
        }
        self.reindex_parents();
        Ok(Undo {
            inverse: vec![Edit::Ungroup { row: first }],
        })
    }

    fn ungroup(&mut self, row: usize) -> Result<Undo, EditError> {
        self.row_exists(row)?;
        let RowKind::Package { template } = self.kind(row) else {
            return Err(EditError::NotAPackage(row));
        };
        let legs = self.children(row);
        let count = legs.len();
        for r in legs {
            self.set_leg_marker(r, false);
        }
        // An empty package has no legs to re-group:
        // `group` refuses `count == 0` (`EmptyInsert`), so `Group` cannot
        // be its inverse. Restore the package's own row instead — a
        // plain `Restore` whose own inverse, `Remove { at: row }`,
        // removes the empty package again, so identity holds both ways.
        if count == 0 {
            let record = self.record(row);
            self.take_out(row);
            self.reindex_parents();
            return Ok(Undo {
                inverse: vec![Edit::Restore {
                    at: row,
                    rows: vec![record],
                }],
            });
        }
        let pkg_id = self.id(row);
        self.take_out(row);
        self.reindex_parents();
        Ok(Undo {
            inverse: vec![Edit::Group {
                first: row,
                count,
                template,
                id: Some(pkg_id),
            }],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sheet::tests::{at, callspread, line, push, result, spx};
    use crate::core::sheet::{Delivered, LineId, LineState, Place, Sheet};
    use geode_core::pricing::{Measure, OptionKind};

    #[test]
    fn undo_of_a_remove_reinstates_rows_with_ids_and_results_and_requests_nothing() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![line(spx(5000.0, OptionKind::Call), 1), callspread(2)],
        );
        for r in [0, 2, 3] {
            assert_eq!(
                s.deliver(s.id(r), 1, Ok(result(10.0 * r as f64 + 1.0)), at(r as i64)),
                Delivered::Installed
            );
        }
        let before: Vec<_> = (0..4).map(|r| s.record(r)).collect();
        let undo = s.apply(Edit::Remove { at: 1 }).unwrap();
        assert_eq!(s.len(), 1);
        // A new insert in between takes a NEW id — the removed ids are never reused.
        push(&mut s, vec![line(spx(5300.0, OptionKind::Put), 1)]);
        assert_eq!(s.id(1), LineId(5));
        s.apply(Edit::Remove { at: 1 }).unwrap();
        // Apply the inverse.
        for e in undo.inverse {
            s.apply(e).unwrap();
        }
        assert_eq!(s.len(), 4);
        let after: Vec<_> = (0..4).map(|r| s.record(r)).collect();
        assert_eq!(
            after, before,
            "ids, results, states and priced_at all return"
        );
        assert_eq!(s.stale_lines().count(), 0, "nothing is re-requested");
        assert_eq!(
            s.state(1),
            &LineState::Fresh,
            "the package folds back to fresh"
        );
        // Restoring an id that is in use is refused.
        let rec = s.record(0);
        assert_eq!(
            s.apply(Edit::Restore {
                at: 4,
                rows: vec![rec]
            })
            .unwrap_err(),
            EditError::IdInUse(LineId(1))
        );
        // Restoring a leg whose parent is gone is refused.
        let mut leg = s.record(2);
        leg.id = LineId(50);
        leg.parent = Some(LineId(60));
        assert_eq!(
            s.apply(Edit::Restore {
                at: 2,
                rows: vec![leg]
            })
            .unwrap_err(),
            EditError::NoSuchParent(LineId(60))
        );
    }

    #[test]
    fn set_instrument_bumps_the_revision_and_stales_only_that_line() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![
                line(spx(5000.0, OptionKind::Call), 1),
                line(spx(5100.0, OptionKind::Call), 1),
            ],
        );
        s.deliver(s.id(0), 1, Ok(result(1.0)), at(0));
        s.deliver(s.id(1), 1, Ok(result(2.0)), at(0));
        assert_eq!(s.stale_lines().count(), 0);
        let undo = s
            .apply(Edit::SetInstrument {
                row: 0,
                instrument: spx(5050.0, OptionKind::Call),
            })
            .unwrap();
        assert_eq!(s.revision(0), 2);
        assert_eq!(s.state(0), &LineState::Stale);
        assert_eq!(
            s.result(0),
            Some(&result(1.0)),
            "the old result stays painted, muted, until the new one lands"
        );
        assert_eq!(s.revision(1), 1);
        assert_eq!(s.state(1), &LineState::Fresh);
        assert_eq!(
            undo.inverse,
            vec![Edit::SetInstrument {
                row: 0,
                instrument: spx(5000.0, OptionKind::Call)
            }]
        );
        // The same instrument again is no change: no bump.
        s.apply(Edit::SetInstrument {
            row: 0,
            instrument: spx(5050.0, OptionKind::Call),
        })
        .unwrap();
        assert_eq!(s.revision(0), 2);
        // Undo restores the old instrument, which IS a request change: re-requested.
        for e in undo.inverse {
            s.apply(e).unwrap();
        }
        assert_eq!(s.revision(0), 3);
        assert_eq!(s.state(0), &LineState::Stale);
        // On a package it is refused.
        push(&mut s, vec![callspread(1)]);
        assert_eq!(
            s.apply(Edit::SetInstrument {
                row: 2,
                instrument: spx(1.0, OptionKind::Call)
            })
            .unwrap_err(),
            EditError::NotALine(2)
        );
        assert_eq!(
            s.apply(Edit::SetInstrument {
                row: 9,
                instrument: spx(1.0, OptionKind::Call)
            })
            .unwrap_err(),
            EditError::NoSuchRow(9)
        );
    }

    #[test]
    fn set_qty_and_move_change_no_request() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![line(spx(5000.0, OptionKind::Call), 1), callspread(1)],
        );
        for r in [0, 2, 3] {
            s.deliver(s.id(r), 1, Ok(result(10.0)), at(0));
        }
        let undo = s.apply(Edit::SetQty { row: 0, qty: -3 }).unwrap();
        assert_eq!(s.qty(0), -3);
        assert_eq!(s.revision(0), 1);
        assert_eq!(s.state(0), &LineState::Fresh);
        assert_eq!(undo.inverse, vec![Edit::SetQty { row: 0, qty: 1 }]);
        // A leg's qty re-sums the package at once (a 1×2 ratio).
        s.apply(Edit::SetQty { row: 3, qty: -2 }).unwrap();
        assert_eq!(s.result(1).unwrap().get(Measure::Npv, false), 10.0 - 20.0);
        assert_eq!(s.state(1), &LineState::Fresh);
        assert_eq!(
            s.apply(Edit::SetQty { row: 0, qty: 0 }).unwrap_err(),
            EditError::ZeroQty
        );
        assert_eq!(
            s.apply(Edit::SetQty { row: 1, qty: 2 }).unwrap_err(),
            EditError::NotALine(1)
        );
        assert_eq!(
            s.stale_lines().count(),
            0,
            "nothing was re-requested by any of it"
        );
    }

    #[test]
    fn set_shift_changes_the_request_only_when_the_effective_value_moves() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        s.deliver(s.id(0), 1, Ok(result(1.0)), at(0));
        let undo = s
            .apply(Edit::SetShift {
                row: 0,
                shift: OwnShifts {
                    spot_pct: Some(2.0),
                    vol_pts: None,
                },
            })
            .unwrap();
        assert_eq!(s.revision(0), 2);
        assert_eq!(s.state(0), &LineState::Stale);
        assert_eq!(
            undo.inverse,
            vec![Edit::SetShift {
                row: 0,
                shift: OwnShifts::default()
            }]
        );
        s.deliver(s.id(0), 2, Ok(result(1.0)), at(1));
        // Setting the own value to what the sheet already gives changes nothing.
        s.apply(Edit::SetSheetShift(OwnShifts {
            spot_pct: Some(2.0),
            vol_pts: None,
        }))
        .unwrap();
        assert_eq!(
            s.revision(0),
            2,
            "own 2.0 over sheet 2.0: the effective value did not move"
        );
        s.apply(Edit::SetShift {
            row: 0,
            shift: OwnShifts::default(),
        })
        .unwrap();
        assert_eq!(
            s.revision(0),
            2,
            "clearing the own value: still 2.0 through the sheet"
        );
        assert_eq!(s.state(0), &LineState::Fresh);
    }

    #[test]
    fn a_sheet_shift_reprices_only_lines_that_inherit_it() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![
                line(spx(5000.0, OptionKind::Call), 1),
                line(spx(5100.0, OptionKind::Call), 1),
                callspread(1),
            ],
        );
        for r in [0, 1, 3, 4] {
            s.deliver(s.id(r), 1, Ok(result(1.0)), at(0));
        }
        s.apply(Edit::SetShift {
            row: 1,
            shift: OwnShifts {
                spot_pct: Some(5.0),
                vol_pts: None,
            },
        })
        .unwrap();
        s.deliver(s.id(1), 2, Ok(result(1.0)), at(1));
        assert_eq!(s.stale_lines().count(), 0);
        let undo = s
            .apply(Edit::SetSheetShift(OwnShifts {
                spot_pct: Some(2.0),
                vol_pts: None,
            }))
            .unwrap();
        assert_eq!(
            s.sheet_shift(),
            OwnShifts {
                spot_pct: Some(2.0),
                vol_pts: None
            }
        );
        assert_eq!(
            s.stale_lines().collect::<Vec<_>>(),
            vec![0, 3, 4],
            "row 1 has its own spot shift"
        );
        assert_eq!(s.revision(1), 2);
        assert_eq!(
            s.state(2),
            &LineState::Stale,
            "the package follows its legs"
        );
        assert_eq!(
            undo.inverse,
            vec![Edit::SetSheetShift(OwnShifts::default())]
        );
        // Vol alone touches the rows that inherit vol — all of them here.
        for r in [0, 3, 4] {
            s.deliver(s.id(r), 2, Ok(result(1.0)), at(2));
        }
        s.apply(Edit::SetSheetShift(OwnShifts {
            spot_pct: Some(2.0),
            vol_pts: Some(-1.0),
        }))
        .unwrap();
        assert_eq!(s.stale_lines().collect::<Vec<_>>(), vec![0, 1, 3, 4]);
    }

    #[test]
    fn a_spot_override_stales_every_line_on_that_underlying_and_only_a_changed_level_does() {
        let mut s = Sheet::new("t");
        let ndx = crate::core::shorthand::parse_builtin("NDX Z26 20000 C").unwrap();
        push(
            &mut s,
            vec![line(spx(5000.0, OptionKind::Call), 1), ndx, callspread(1)],
        );
        for r in [0, 1, 3, 4] {
            s.deliver(s.id(r), 1, Ok(result(1.0)), at(0));
        }
        let undo = s
            .apply(Edit::SetSpotOverride {
                underlying: "SPX".into(),
                level: Some(5100.0),
            })
            .unwrap();
        assert_eq!(s.overrides().spot.get("SPX"), Some(&5100.0));
        assert_eq!(
            s.stale_lines().collect::<Vec<_>>(),
            vec![0, 3, 4],
            "NDX is untouched"
        );
        assert_eq!(s.revision(0), 2);
        assert_eq!(s.revision(1), 1);
        assert_eq!(
            undo.inverse,
            vec![Edit::SetSpotOverride {
                underlying: "SPX".into(),
                level: None
            }]
        );
        for r in [0, 3, 4] {
            s.deliver(s.id(r), 2, Ok(result(1.0)), at(1));
        }
        // The same level again is no change.
        s.apply(Edit::SetSpotOverride {
            underlying: "SPX".into(),
            level: Some(5100.0),
        })
        .unwrap();
        assert_eq!(s.stale_lines().count(), 0);
        // Clearing an override that is not set is no change either.
        s.apply(Edit::SetSpotOverride {
            underlying: "RTY".into(),
            level: None,
        })
        .unwrap();
        assert_eq!(s.stale_lines().count(), 0);
        assert!(!s.overrides().spot.contains_key("RTY"));
        // Clearing SPX stales SPX again and the inverse carries the old level.
        let undo = s
            .apply(Edit::SetSpotOverride {
                underlying: "SPX".into(),
                level: None,
            })
            .unwrap();
        assert_eq!(s.stale_lines().collect::<Vec<_>>(), vec![0, 3, 4]);
        assert_eq!(
            undo.inverse,
            vec![Edit::SetSpotOverride {
                underlying: "SPX".into(),
                level: Some(5100.0)
            }]
        );
        // The underlying is matched case-insensitively, stored upper-case.
        s.apply(Edit::SetSpotOverride {
            underlying: "ndx".into(),
            level: Some(1.0),
        })
        .unwrap();
        assert_eq!(s.overrides().spot.get("NDX"), Some(&1.0));
        assert_eq!(s.state(1), &LineState::Stale);
    }

    #[test]
    fn insert_then_its_inverse_is_identity_for_roots_and_legs() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![callspread(1)]);
        let before: Vec<_> = (0..s.len()).map(|r| s.record(r)).collect();
        let undo = s
            .apply(Edit::Insert {
                place: Place::Root { at: 0 },
                rows: vec![line(spx(1.0, OptionKind::Call), 1), callspread(2)],
            })
            .unwrap();
        assert_eq!(s.len(), 7);
        assert_eq!(
            undo.inverse,
            vec![Edit::Remove { at: 0 }, Edit::Remove { at: 0 }]
        );
        for e in undo.inverse {
            s.apply(e).unwrap();
        }
        assert_eq!(
            (0..s.len()).map(|r| s.record(r)).collect::<Vec<_>>(),
            before
        );
        let undo = s
            .apply(Edit::Insert {
                place: Place::Leg { package: 0, leg: 1 },
                rows: vec![
                    line(spx(2.0, OptionKind::Put), 1),
                    line(spx(3.0, OptionKind::Put), 1),
                ],
            })
            .unwrap();
        assert_eq!(s.children(0), 1..5);
        assert_eq!(
            undo.inverse,
            vec![
                Edit::Remove { at: 2 },
                Edit::Remove { at: 2 },
                // Four legs fit no table: the name comes back after.
                Edit::SetTemplate {
                    row: 0,
                    template: Template::CS
                }
            ]
        );
        for e in undo.inverse {
            s.apply(e).unwrap();
        }
        assert_eq!(
            (0..s.len()).map(|r| s.record(r)).collect::<Vec<_>>(),
            before
        );
    }

    fn ids(s: &Sheet) -> Vec<u64> {
        (0..s.len()).map(|r| s.id(r).0).collect()
    }

    #[test]
    fn move_stays_within_the_parent_and_carries_a_packages_legs() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![
                line(spx(1.0, OptionKind::Call), 1),
                callspread(1),
                line(spx(2.0, OptionKind::Call), 1),
            ],
        );
        // ids: 1 | 2 (3 4) | 5
        let undo = s.apply(Edit::Move { row: 0, delta: 1 }).unwrap();
        assert_eq!(
            ids(&s),
            vec![2, 3, 4, 1, 5],
            "the line hops the whole package"
        );
        assert_eq!(s.parent(1), Some(0));
        assert_eq!(s.parent(2), Some(0));
        assert_eq!(undo.inverse, vec![Edit::Move { row: 3, delta: -1 }]);
        for e in undo.inverse {
            s.apply(e).unwrap();
        }
        assert_eq!(ids(&s), vec![1, 2, 3, 4, 5]);
        // The package moves down, legs with it.
        s.apply(Edit::Move { row: 1, delta: 1 }).unwrap();
        assert_eq!(ids(&s), vec![1, 5, 2, 3, 4]);
        assert_eq!(s.children(2), 3..5);
        // A leg moves within its package only.
        s.apply(Edit::Move { row: 3, delta: 1 }).unwrap();
        assert_eq!(ids(&s), vec![1, 5, 2, 4, 3]);
        assert_eq!(
            s.apply(Edit::Move { row: 4, delta: 1 }).unwrap_err(),
            EditError::MoveOffEnd
        );
        s.apply(Edit::Move { row: 4, delta: -1 }).unwrap();
        assert_eq!(ids(&s), vec![1, 5, 2, 3, 4]);
        assert_eq!(
            s.apply(Edit::Move { row: 3, delta: -1 }).unwrap_err(),
            EditError::MoveOffEnd,
            "the first leg cannot leave the package"
        );
        assert_eq!(
            s.apply(Edit::Move { row: 0, delta: -1 }).unwrap_err(),
            EditError::MoveOffEnd
        );
        assert_eq!(
            s.apply(Edit::Move { row: 2, delta: 1 }).unwrap_err(),
            EditError::MoveOffEnd,
            "the last root"
        );
        // A delta of 2 is two hops.
        s.apply(Edit::Move { row: 0, delta: 2 }).unwrap();
        assert_eq!(ids(&s), vec![5, 2, 3, 4, 1]);
        assert_eq!(
            s.apply(Edit::Move { row: 9, delta: 1 }).unwrap_err(),
            EditError::NoSuchRow(9)
        );
        assert_eq!(
            s.stale_lines().count(),
            4,
            "still stale from insertion: a move changes no request \
             (4 of the 5 rows are lines; the package row is Fresh from creation)"
        );
        assert!((0..5).all(|r| s.revision(r) == 1));
    }

    #[test]
    fn group_makes_a_custom_package_of_a_contiguous_run_of_root_lines() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![
                line(spx(1.0, OptionKind::Call), 1),
                line(spx(2.0, OptionKind::Put), -1),
                line(spx(3.0, OptionKind::Call), 1),
            ],
        );
        for r in 0..3 {
            s.deliver(s.id(r), 1, Ok(result(10.0)), at(0));
        }
        let undo = s
            .apply(Edit::Group {
                first: 0,
                count: 2,
                template: Template::CUSTOM,
                id: None,
            })
            .unwrap();
        assert_eq!(s.len(), 4);
        assert_eq!(
            s.kind(0),
            RowKind::Package {
                template: Template::CUSTOM
            }
        );
        assert_eq!(s.id(0), LineId(4), "a fresh id");
        assert_eq!(s.children(0), 1..3);
        assert_eq!(ids(&s), vec![4, 1, 2, 3]);
        assert_eq!(s.result(0).unwrap().get(Measure::Npv, false), 10.0 - 10.0);
        assert_eq!(
            s.state(0),
            &LineState::Fresh,
            "grouping re-requests nothing"
        );
        assert_eq!(s.stale_lines().count(), 0);
        assert_eq!(undo.inverse, vec![Edit::Ungroup { row: 0 }]);
        let redo = s.undo(&undo).unwrap();
        assert_eq!(ids(&s), vec![1, 2, 3]);
        assert!(s.roots().eq(0..3));
        assert_eq!(
            redo.inverse,
            vec![Edit::Group {
                first: 0,
                count: 2,
                template: Template::CUSTOM,
                id: Some(LineId(4))
            }]
        );
        s.undo(&redo).unwrap();
        assert_eq!(
            ids(&s),
            vec![4, 1, 2, 3],
            "redo restores the same package id"
        );
    }

    #[test]
    fn group_refuses_a_run_that_is_not_contiguous_roots() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![
                line(spx(1.0, OptionKind::Call), 1),
                callspread(1),
                line(spx(2.0, OptionKind::Call), 1),
            ],
        );
        // Over a package.
        assert_eq!(
            s.apply(Edit::Group {
                first: 0,
                count: 2,
                template: Template::CUSTOM,
                id: None
            })
            .unwrap_err(),
            EditError::NotContiguousRoots
        );
        // Starting on a leg.
        assert_eq!(
            s.apply(Edit::Group {
                first: 2,
                count: 1,
                template: Template::CUSTOM,
                id: None
            })
            .unwrap_err(),
            EditError::NotContiguousRoots
        );
        // Past the end.
        assert_eq!(
            s.apply(Edit::Group {
                first: 4,
                count: 2,
                template: Template::CUSTOM,
                id: None
            })
            .unwrap_err(),
            EditError::NotContiguousRoots
        );
        assert_eq!(
            s.apply(Edit::Group {
                first: 0,
                count: 0,
                template: Template::CUSTOM,
                id: None
            })
            .unwrap_err(),
            EditError::EmptyInsert
        );
        assert_eq!(
            s.apply(Edit::Group {
                first: 9,
                count: 1,
                template: Template::CUSTOM,
                id: None
            })
            .unwrap_err(),
            EditError::NoSuchRow(9)
        );
        // An id in use is refused.
        assert_eq!(
            s.apply(Edit::Group {
                first: 0,
                count: 1,
                template: Template::CUSTOM,
                id: Some(LineId(1))
            })
            .unwrap_err(),
            EditError::IdInUse(LineId(1))
        );
        assert_eq!(s.len(), 5, "nothing changed");
        // A single root is a valid group.
        s.apply(Edit::Group {
            first: 4,
            count: 1,
            template: Template::CUSTOM,
            id: None,
        })
        .unwrap();
        assert_eq!(s.children(4), 5..6);
    }

    #[test]
    fn ungroup_promotes_the_legs_in_place_and_refuses_a_line() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![
                line(spx(1.0, OptionKind::Call), 1),
                callspread(2),
                line(spx(2.0, OptionKind::Call), 1),
            ],
        );
        let undo = s.apply(Edit::Ungroup { row: 1 }).unwrap();
        assert_eq!(ids(&s), vec![1, 3, 4, 5]);
        assert!(s.roots().eq(0..4));
        assert_eq!(
            undo.inverse,
            vec![Edit::Group {
                first: 1,
                count: 2,
                template: Template::CS,
                id: Some(LineId(2))
            }]
        );
        assert_eq!(
            s.apply(Edit::Ungroup { row: 0 }).unwrap_err(),
            EditError::NotAPackage(0)
        );
        assert_eq!(
            s.apply(Edit::Ungroup { row: 9 }).unwrap_err(),
            EditError::NoSuchRow(9)
        );
        s.undo(&undo).unwrap();
        assert_eq!(ids(&s), vec![1, 2, 3, 4, 5]);
        assert_eq!(
            s.kind(1),
            RowKind::Package {
                template: Template::CS
            },
            "the template survives the round trip"
        );
        // An empty package ungroups to nothing, and undo restores it
        // (CUSTOM since its legs left: no legs are no structure).
        s.apply(Edit::Remove { at: 2 }).unwrap();
        s.apply(Edit::Remove { at: 2 }).unwrap();
        let undo = s.apply(Edit::Ungroup { row: 1 }).unwrap();
        assert_eq!(ids(&s), vec![1, 5]);
        let redo = s.undo(&undo).unwrap();
        assert_eq!(ids(&s), vec![1, 2, 5]);
        assert_eq!(s.id(1), LineId(2), "the same package id returns");
        assert_eq!(
            s.kind(1),
            RowKind::Package {
                template: Template::CUSTOM
            }
        );
        assert_eq!(s.children(1), 2..2, "still no legs");
        assert_eq!(redo.inverse, vec![Edit::Remove { at: 1 }]);
    }

    fn tag(s: &Sheet, row: usize) -> Template {
        match s.kind(row) {
            RowKind::Package { template } => template,
            k => panic!("row {row} is {k:?}, not a package"),
        }
    }

    fn sheet_of(text: &str) -> Sheet {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![crate::core::shorthand::parse_builtin(text).unwrap()],
        );
        s
    }

    /// The builtin tables with `extra` (config TOML) layered over them.
    fn templates_with(extra: &str) -> std::sync::Arc<crate::core::template::TemplateSet> {
        use crate::core::template::{BUILTIN_TEMPLATES, PRICER_TEMPLATES_DOC, TemplateSet};
        use geode_core::config::{LayerDoc, merge_docs};
        let builtin = LayerDoc::builtin(PRICER_TEMPLATES_DOC, BUILTIN_TEMPLATES).unwrap();
        let user = LayerDoc::builtin(PRICER_TEMPLATES_DOC, extra).unwrap();
        std::sync::Arc::new(
            TemplateSet::from_doc(&merge_docs(PRICER_TEMPLATES_DOC, &[builtin, user])).0,
        )
    }

    #[test]
    fn a_leg_edit_renames_the_package_to_the_structure_its_legs_now_form() {
        // RR: -1 4800 P, +1 5200 C. The put leg turned call: -1 4800 C,
        // +1 5200 C is a short call spread.
        let mut s = sheet_of("SPX Z26 4800/5200 RR");
        assert_eq!(tag(&s, 0), Template::RR);
        let undo = s
            .apply(Edit::SetInstrument {
                row: 1,
                instrument: spx(4800.0, OptionKind::Call),
            })
            .unwrap();
        assert_eq!(tag(&s, 0), Template::CS);
        assert_eq!(s.shorthand(0), "-1 SPX Z26 4800/5200 CS");
        let redo = s.undo(&undo).unwrap();
        assert_eq!(tag(&s, 0), Template::RR, "undo restores the name");
        s.undo(&redo).unwrap();
        assert_eq!(tag(&s, 0), Template::CS, "redo renames again");
    }

    #[test]
    fn legs_that_fit_no_table_make_the_package_custom() {
        let mut s = sheet_of("SPX Z26 4800/5200 CS");
        // A 1x2: no table's weights.
        let undo = s.apply(Edit::SetQty { row: 2, qty: -2 }).unwrap();
        assert_eq!(tag(&s, 0), Template::CUSTOM);
        s.undo(&undo).unwrap();
        assert_eq!(tag(&s, 0), Template::CS);
        // A leg removed, then put back by undo.
        let undo = s.apply(Edit::Remove { at: 2 }).unwrap();
        assert_eq!(tag(&s, 0), Template::CUSTOM, "one leg is no structure");
        s.undo(&undo).unwrap();
        assert_eq!(tag(&s, 0), Template::CS);
        // A third leg inserted.
        s.apply(Edit::Insert {
            place: Place::Leg { package: 0, leg: 2 },
            rows: vec![line(spx(5600.0, OptionKind::Call), 1)],
        })
        .unwrap();
        assert_eq!(tag(&s, 0), Template::CUSTOM);
    }

    #[test]
    fn a_leg_edit_that_completes_a_structure_names_it_and_moving_legs_rereads() {
        // A custom package whose put leg turns call becomes a call spread.
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![
                line(spx(4800.0, OptionKind::Call), 1),
                line(spx(5200.0, OptionKind::Put), -1),
            ],
        );
        s.apply(Edit::Group {
            first: 0,
            count: 2,
            template: Template::CUSTOM,
            id: None,
        })
        .unwrap();
        assert_eq!(tag(&s, 0), Template::CUSTOM);
        s.apply(Edit::SetInstrument {
            row: 2,
            instrument: spx(5200.0, OptionKind::Call),
        })
        .unwrap();
        assert_eq!(tag(&s, 0), Template::CS);
        // Legs read in sheet order, as the shorthand prints them: a risk
        // reversal with its call first fits no table.
        let mut s = sheet_of("SPX Z26 4800/5200 RR");
        s.apply(Edit::Move { row: 1, delta: 1 }).unwrap();
        assert_eq!(tag(&s, 0), Template::CUSTOM);
        s.apply(Edit::Move { row: 1, delta: 1 }).unwrap();
        assert_eq!(tag(&s, 0), Template::RR);
    }

    #[test]
    fn grouping_lines_that_form_a_structure_names_it() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![
                line(spx(4800.0, OptionKind::Put), -1),
                line(spx(5200.0, OptionKind::Call), 1),
            ],
        );
        let undo = s
            .apply(Edit::Group {
                first: 0,
                count: 2,
                template: Template::CUSTOM,
                id: None,
            })
            .unwrap();
        assert_eq!(tag(&s, 0), Template::RR);
        assert_eq!(undo.inverse, vec![Edit::Ungroup { row: 0 }]);
        let redo = s.undo(&undo).unwrap();
        assert_eq!(
            redo.inverse,
            vec![Edit::Group {
                first: 0,
                count: 2,
                template: Template::RR,
                id: Some(LineId(3))
            }]
        );
    }

    #[test]
    fn a_name_the_legs_still_fit_is_kept_over_an_identical_earlier_table() {
        // RISKREV repeats RR's table after it: a package typed RISKREV
        // stays RISKREV through a strike change.
        let set = templates_with(
            "[RISKREV]\nlegs = [ { weight = -1, strike = 1, kind = \"P\" }, { weight = 1, strike = 2, kind = \"C\" } ]\n",
        );
        let mut s = Sheet::new("t");
        s.set_templates(set.clone());
        push(
            &mut s,
            vec![crate::core::shorthand::parse("SPX Z26 4800/5200 RISKREV", &set).unwrap()],
        );
        s.apply(Edit::SetInstrument {
            row: 2,
            instrument: spx(5300.0, OptionKind::Call),
        })
        .unwrap();
        assert_eq!(tag(&s, 0), Template::named("RISKREV"));
    }

    #[test]
    fn a_stale_name_survives_until_a_leg_edit_and_comes_back_on_undo() {
        // A reload redefines RR as long both legs: the stored RR no longer
        // fits it, and keeps its name until its legs change.
        let mut s = sheet_of("SPX Z26 4800/5200 RR");
        s.set_templates(templates_with(
            "[RR]\nlegs = [ { weight = 1, strike = 1, kind = \"P\" }, { weight = 1, strike = 2, kind = \"C\" } ]\n",
        ));
        s.apply(Edit::SetShift {
            row: 1,
            shift: OwnShifts {
                spot_pct: Some(1.0),
                vol_pts: None,
            },
        })
        .unwrap();
        assert_eq!(tag(&s, 0), Template::RR, "a shift leaves the legs' shape");
        let undo = s
            .apply(Edit::SetInstrument {
                row: 1,
                instrument: spx(4800.0, OptionKind::Call),
            })
            .unwrap();
        assert_eq!(tag(&s, 0), Template::CS);
        let redo = s.undo(&undo).unwrap();
        assert_eq!(tag(&s, 0), Template::RR, "undo restores the stored name");
        let undo = s.undo(&redo).unwrap();
        assert_eq!(tag(&s, 0), Template::CS);
        s.undo(&undo).unwrap();
        assert_eq!(tag(&s, 0), Template::RR);
    }

    /// Each edit and its inverse restore row contents; request changes still advance revisions.
    #[test]
    fn apply_then_undo_is_identity_for_every_edit() {
        fn fixture() -> Sheet {
            let mut s = Sheet::new("t");
            push(
                &mut s,
                vec![
                    line(spx(1.0, OptionKind::Call), 1),
                    callspread(2),
                    line(spx(2.0, OptionKind::Put), -1),
                    line(spx(3.0, OptionKind::Call), 1),
                ],
            );
            for r in [0, 2, 3, 4, 5] {
                s.deliver(s.id(r), 1, Ok(result(r as f64)), at(r as i64));
            }
            s.apply(Edit::SetSpotOverride {
                underlying: "SPX".into(),
                level: Some(5000.0),
            })
            .unwrap();
            for r in [0, 2, 3, 4, 5] {
                s.deliver(s.id(r), 2, Ok(result(r as f64)), at(10 + r as i64));
            }
            // An empty package, at row 6: push one more
            // callspread then remove both its legs, leaving the package
            // row with no children.
            push(&mut s, vec![callspread(1)]);
            s.apply(Edit::Remove { at: 7 }).unwrap();
            s.apply(Edit::Remove { at: 7 }).unwrap();
            s
        }
        fn snapshot(s: &Sheet) -> (Vec<RowRecord>, OwnShifts, Vec<(String, f64)>) {
            (
                (0..s.len()).map(|r| s.record(r)).collect(),
                s.sheet_shift(),
                s.overrides()
                    .spot
                    .iter()
                    .map(|(k, v)| (k.clone(), *v))
                    .collect(),
            )
        }
        let edits = vec![
            Edit::Insert {
                place: Place::Root { at: 1 },
                rows: vec![line(spx(9.0, OptionKind::Call), 1), callspread(1)],
            },
            Edit::Insert {
                place: Place::Leg { package: 1, leg: 0 },
                rows: vec![line(spx(9.0, OptionKind::Call), 1)],
            },
            Edit::Remove { at: 1 },
            Edit::Remove { at: 2 },
            Edit::SetInstrument {
                row: 0,
                instrument: spx(7.0, OptionKind::Put),
            },
            Edit::SetQty { row: 2, qty: 5 },
            Edit::SetShift {
                row: 3,
                shift: OwnShifts {
                    spot_pct: Some(1.0),
                    vol_pts: Some(2.0),
                },
            },
            Edit::Move { row: 0, delta: 1 },
            Edit::Move { row: 2, delta: 1 },
            Edit::Group {
                first: 4,
                count: 2,
                template: Template::CUSTOM,
                id: None,
            },
            Edit::Ungroup { row: 1 },
            Edit::Ungroup { row: 6 },
            Edit::Remove { at: 6 },
            Edit::SetSheetShift(OwnShifts {
                spot_pct: Some(3.0),
                vol_pts: None,
            }),
            Edit::SetSpotOverride {
                underlying: "SPX".into(),
                level: Some(5200.0),
            },
            Edit::SetSpotOverride {
                underlying: "SPX".into(),
                level: None,
            },
            Edit::SetSpotOverride {
                underlying: "NDX".into(),
                level: Some(1.0),
            },
        ];
        for edit in edits {
            let mut s = fixture();
            let before = snapshot(&s);
            let label = format!("{edit:?}");
            let undo = s.apply(edit).unwrap_or_else(|e| panic!("{label}: {e}"));
            let redo = s
                .undo(&undo)
                .unwrap_or_else(|e| panic!("undo of {label}: {e}"));
            assert!(
                !redo.inverse.is_empty(),
                "{label}: an undo always has a redo"
            );
            let after = snapshot(&s);
            // Revisions may have moved (a request change and its reversal
            // are two bumps) and states may be Stale; ids, kinds, parents,
            // instruments, quantities, shifts, results and priced_at are identical.
            assert_eq!(after.0.len(), before.0.len(), "{label}");
            for (a, b) in after.0.iter().zip(&before.0) {
                assert_eq!(
                    (
                        a.id,
                        a.kind,
                        a.parent,
                        &a.instrument,
                        a.qty,
                        a.shift,
                        a.result,
                        a.priced_at
                    ),
                    (
                        b.id,
                        b.kind,
                        b.parent,
                        &b.instrument,
                        b.qty,
                        b.shift,
                        b.result,
                        b.priced_at
                    ),
                    "{label}"
                );
            }
            assert_eq!(after.1, before.1, "{label}");
            assert_eq!(after.2, before.2, "{label}");
        }
    }
}
