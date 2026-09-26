//! The tile's undo history (line-pricer spec §6.2, §8.5's `u`/`ctrl+r`):
//! at most [`UNDO_DEPTH`] entries, strictly LIFO. Every inverse is
//! recorded against the exact rows the edit left, so NOTHING may edit
//! the sheet except through a recorded `apply` — a `Restore` trusts its
//! records' state and revision (spec §17's Part 3 obligation). When an
//! inverse is refused anyway, `Sheet::undo` is not atomic, so the whole
//! history is dropped rather than left pointing at rows that moved.

use crate::core::edit::{EditError, Undo};
use crate::core::sheet::Sheet;
use std::collections::VecDeque;

pub const UNDO_DEPTH: usize = 100;

#[derive(Debug, Default)]
pub struct UndoStack {
    done: VecDeque<Undo>,
    undone: Vec<Undo>,
}

impl UndoStack {
    /// A fresh edit: onto the done side, the redo side forgotten (a new
    /// edit forks history), the oldest entry dropped past the depth.
    pub fn record(&mut self, undo: Undo) {
        self.undone.clear();
        self.done.push_back(undo);
        if self.done.len() > UNDO_DEPTH {
            self.done.pop_front();
        }
    }

    /// Take back the newest edit. `Ok(false)` when there is none.
    pub fn undo(&mut self, sheet: &mut Sheet) -> Result<bool, EditError> {
        let Some(undo) = self.done.pop_back() else {
            return Ok(false);
        };
        match sheet.undo(&undo) {
            Ok(redo) => {
                self.undone.push(redo);
                Ok(true)
            }
            Err(e) => {
                self.clear();
                Err(e)
            }
        }
    }

    /// Re-apply the newest undone edit. `Ok(false)` when there is none.
    pub fn redo(&mut self, sheet: &mut Sheet) -> Result<bool, EditError> {
        let Some(redo) = self.undone.pop() else {
            return Ok(false);
        };
        match sheet.undo(&redo) {
            Ok(undo) => {
                self.done.push_back(undo);
                Ok(true)
            }
            Err(e) => {
                self.clear();
                Err(e)
            }
        }
    }

    /// The entry the next `undo` (or, with `redo`, the next `redo`)
    /// replays, without replaying it.
    pub fn peek(&self, redo: bool) -> Option<&Undo> {
        if redo {
            self.undone.last()
        } else {
            self.done.back()
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    pub fn clear(&mut self) {
        self.done.clear();
        self.undone.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::edit::Edit;
    use crate::core::sheet::tests::{at, callspread, line, push, result, spx};
    use crate::core::sheet::{LineId, Place};
    use geode_core::pricing::OptionKind;

    fn ids(s: &Sheet) -> Vec<LineId> {
        (0..s.len()).map(|r| s.id(r)).collect()
    }

    fn apply(stack: &mut UndoStack, s: &mut Sheet, e: Edit) {
        let undo = s.apply(e).unwrap();
        stack.record(undo);
    }

    #[test]
    fn a_package_hopping_upward_undoes_and_redoes_to_the_same_order() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        push(&mut s, vec![callspread(1)]);
        let before = ids(&s);
        let mut stack = UndoStack::default();
        apply(&mut stack, &mut s, Edit::Move { row: 2, delta: -1 });
        let moved = ids(&s);
        assert_eq!(s.children(1), 2..4, "the package moved up with its legs");
        assert_ne!(moved, before);

        assert_eq!(stack.undo(&mut s), Ok(true));
        assert_eq!(ids(&s), before);
        assert_eq!(stack.redo(&mut s), Ok(true));
        assert_eq!(ids(&s), moved, "redo lands the same order, legs contiguous");
        assert_eq!(stack.undo(&mut s), Ok(true));
        assert_eq!(ids(&s), before, "and it undoes again");
    }

    #[test]
    fn a_multi_edit_undo_redoes_whole_with_the_same_ids_and_results() {
        let mut s = Sheet::new("t");
        let mut stack = UndoStack::default();
        apply(
            &mut stack,
            &mut s,
            Edit::Insert {
                place: Place::Root { at: 0 },
                rows: vec![
                    line(spx(5000.0, OptionKind::Call), 1),
                    callspread(-2),
                    line(spx(4000.0, OptionKind::Put), 3),
                ],
            },
        );
        let answers: Vec<_> = (0..s.len())
            .filter(|r| s.is_line(*r))
            .map(|r| (s.id(r), s.revision(r), Ok(result(r as f64 + 1.0))))
            .collect();
        s.deliver_all(answers, at(0));
        let before = ids(&s);
        let prices: Vec<_> = (0..s.len()).map(|r| s.result(r).map(|x| x.price)).collect();

        assert_eq!(stack.undo(&mut s), Ok(true));
        assert!(s.is_empty(), "one undo takes back the whole insert");
        assert_eq!(stack.redo(&mut s), Ok(true));
        assert_eq!(ids(&s), before, "redo reinstates the same ids");
        let after: Vec<_> = (0..s.len()).map(|r| s.result(r).map(|x| x.price)).collect();
        assert_eq!(after, prices, "…and their results: nothing is re-requested");
        assert!(s.stale_lines().next().is_none());
    }

    #[test]
    fn a_new_edit_clears_the_redo_side() {
        let mut s = Sheet::new("t");
        let mut stack = UndoStack::default();
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        apply(&mut stack, &mut s, Edit::SetQty { row: 0, qty: 2 });
        stack.undo(&mut s).unwrap();
        assert!(stack.can_redo());
        apply(&mut stack, &mut s, Edit::SetQty { row: 0, qty: 5 });
        assert!(!stack.can_redo(), "a fresh edit forks history");
        assert_eq!(stack.redo(&mut s), Ok(false));
        assert_eq!(s.qty(0), 5);
    }

    #[test]
    fn the_stack_keeps_the_newest_hundred() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        let mut stack = UndoStack::default();
        for q in 2..(UNDO_DEPTH as i64 + 12) {
            apply(&mut stack, &mut s, Edit::SetQty { row: 0, qty: q });
        }
        let mut n = 0;
        while stack.undo(&mut s).unwrap() {
            n += 1;
        }
        assert_eq!(n, UNDO_DEPTH);
        assert_eq!(s.qty(0), 11, "the oldest eleven edits fell off the bottom");
    }

    /// Why every tile edit goes through the stack (Part 3 global
    /// constraints): an inverse recorded against rows that moved behind
    /// the stack's back is refused, `Sheet::undo` is not atomic, and the
    /// only safe state afterwards is an empty history.
    #[test]
    fn an_inverse_refused_mid_undo_clears_both_sides() {
        let mut s = Sheet::new("t");
        let mut stack = UndoStack::default();
        apply(
            &mut stack,
            &mut s,
            Edit::Insert {
                place: Place::Root { at: 0 },
                rows: vec![line(spx(5000.0, OptionKind::Call), 1)],
            },
        );
        s.apply(Edit::Remove { at: 0 }).unwrap(); // behind the stack's back
        assert!(stack.undo(&mut s).is_err());
        assert!(!stack.can_undo() && !stack.can_redo());
    }

    /// The single-entry case above empties both sides regardless of
    /// whether the failure path clears them: the erroring entry was
    /// already popped and nothing had reached `undone`. With a second
    /// edit still on `done` and a prior undo already sitting on `undone`,
    /// only an explicit clear on the error path drops them too.
    #[test]
    fn a_refused_inverse_mid_undo_drops_the_rest_of_both_sides() {
        let mut s = Sheet::new("t");
        let mut stack = UndoStack::default();
        apply(
            &mut stack,
            &mut s,
            Edit::Insert {
                place: Place::Root { at: 0 },
                rows: vec![line(spx(5000.0, OptionKind::Call), 1)],
            },
        );
        apply(
            &mut stack,
            &mut s,
            Edit::Insert {
                place: Place::Root { at: 1 },
                rows: vec![line(spx(4000.0, OptionKind::Put), 1)],
            },
        );
        assert_eq!(stack.undo(&mut s), Ok(true), "undoes the second insert");
        assert!(stack.can_redo(), "its redo now sits on the undone side");
        s.apply(Edit::Remove { at: 0 }).unwrap(); // behind the stack's back
        assert!(
            stack.undo(&mut s).is_err(),
            "the first insert's inverse no longer matches"
        );
        assert!(
            !stack.can_undo(),
            "the remaining done entry does not survive"
        );
        assert!(!stack.can_redo(), "nor does the earlier redo");
    }

    /// A refused redo clears both history stacks. This fixture duplicates the next
    /// Restore target outside the history stack, making that replay invalid while other
    /// undo and redo entries still exist.
    #[test]
    fn a_refused_redo_drops_the_rest_of_both_sides() {
        let mut s = Sheet::new("t");
        let mut stack = UndoStack::default();
        for (at, strike) in [(0, 5000.0), (1, 4000.0), (2, 3000.0)] {
            apply(
                &mut stack,
                &mut s,
                Edit::Insert {
                    place: Place::Root { at },
                    rows: vec![line(spx(strike, OptionKind::Call), 1)],
                },
            );
        }
        assert_eq!(stack.undo(&mut s), Ok(true));
        assert_eq!(stack.undo(&mut s), Ok(true));
        assert_eq!(s.len(), 1);
        assert!(stack.can_undo(), "fixture: the first insert is still done");
        // Restore the next redo's target directly so replay must refuse the duplicate.
        let next = stack.peek(true).expect("two redos wait").inverse[0].clone();
        s.apply(next).unwrap();
        assert!(
            matches!(stack.redo(&mut s), Err(EditError::IdInUse(_))),
            "the redo's line is already in the sheet"
        );
        assert!(!stack.can_redo(), "the second redo does not survive");
        assert!(!stack.can_undo(), "nor does the done entry");
    }
}
