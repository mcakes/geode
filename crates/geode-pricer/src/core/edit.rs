//! The one mutation door (line-pricer spec §6.2): every change to a
//! sheet is an [`Edit`], `apply` answers the inverse as an [`Undo`], and
//! decides which lines are re-requested by comparing `Sheet::request`
//! before and after (spec §9.3). Undo of a removal reinstates ids and
//! results (a `Restore`), so it requests nothing.

use crate::core::sheet::{LineId, LineSpec, OwnShifts, Place, RowKind, RowRecord, RowSpec, Sheet};
use crate::core::template::Template;
use geode_core::pricing::{Instrument, PriceRequest};
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    /// A line, or a package with its legs, or several roots (decision 1).
    Insert {
        place: Place,
        rows: Vec<RowSpec>,
    },
    /// A package removes its legs.
    Remove {
        at: usize,
    },
    /// The inverse of `Remove`: rows back with their ids, results and
    /// states (decision 3). A caller other than `undo` has no reason to
    /// build one.
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
    SetSheetShift(OwnShifts),
    /// `None` clears (spec ruling 1).
    SetSpotOverride {
        underlying: String,
        level: Option<f64>,
    },
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
    /// A package spec at a leg place: depth is at most two (ruling 5).
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
    /// The footer's text (spec §6.2, §8.3).
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
    /// The one door (spec §6.2). Bumps `revision` and sets `Stale` on
    /// every line whose `request()` changed (spec §9.3), folds packages,
    /// and answers the inverse. On an error nothing has changed.
    pub fn apply(&mut self, edit: Edit) -> Result<Undo, EditError> {
        let touched = self.touched_by(&edit);
        let before: Vec<(LineId, Option<PriceRequest>)> = touched
            .iter()
            .map(|id| (*id, self.index_of(*id).and_then(|r| self.request(r))))
            .collect();
        let undo = self.apply_inner(edit)?;
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

    /// The lines whose request an edit CAN change, by id (so the compare
    /// survives the edit moving rows). `SetSpotOverride` is not here: its
    /// request is unchanged by design (§9.3) and `apply_inner` stales
    /// its lines explicitly (decision 4).
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
            | Edit::SetSpotOverride { .. } => Vec::new(),
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
            Edit::Move {
                row: _row,
                delta: _delta,
            } => todo!("Task 6"),
            Edit::Group {
                first: _first,
                count: _count,
                template: _template,
                id: _id,
            } => todo!("Task 6"),
            Edit::Ungroup { row: _row } => todo!("Task 6"),
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
                    // The request is unchanged by design (§9.3: overrides
                    // ride in `PriceParams`), so the compare in `apply`
                    // cannot see this; stale the lines explicitly.
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
        // Every leg's parent must be a package in the sheet or in this batch.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sheet::tests::{at, callspread, line, push, result, spx};
    use crate::core::sheet::{Delivered, LineId, LineState, Place, Sheet};
    use geode_core::pricing::OptionKind;

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
        // Undo restores the old instrument, which IS a request change: re-requested (spec §9.3).
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
        // A leg's qty re-sums the package at once (a 1×2 ratio, spec §6.4).
        s.apply(Edit::SetQty { row: 3, qty: -2 }).unwrap();
        assert_eq!(s.result(1).unwrap().price, 10.0 - 20.0);
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
            s.sheet_shift,
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
        let ndx = crate::core::shorthand::parse("NDX Z26 20000 C").unwrap();
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
        assert_eq!(s.overrides.spot.get("SPX"), Some(&5100.0));
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
        assert!(!s.overrides.spot.contains_key("RTY"));
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
        assert_eq!(s.overrides.spot.get("NDX"), Some(&1.0));
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
            vec![Edit::Remove { at: 2 }, Edit::Remove { at: 2 }]
        );
        for e in undo.inverse {
            s.apply(e).unwrap();
        }
        assert_eq!(
            (0..s.len()).map(|r| s.record(r)).collect::<Vec<_>>(),
            before
        );
    }
}
