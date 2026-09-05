//! The cursor (Phase 3 spec §6.1): a visible-row and column pair driven
//! by the shell's `vimnav` vocabulary, multiplied by the engine's count.

use crate::core::expansion::path_of;
use crate::core::plan::ColumnPlan;
use geode_core::snapshot::Snapshot;
use geode_shell::vimnav::{NavCommand, apply};
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Normal,
    Visual {
        anchor: usize,
    },
}

impl Cursor {
    pub fn move_rows(&mut self, len: usize, cmd: NavCommand, count: Option<u32>) {
        let n = count.unwrap_or(1) as i64;
        let cmd = match (cmd, count) {
            (NavCommand::Move(d), _) => NavCommand::Move(d * n),
            // `12G` is "row 12", vim-style, 1-based.
            (NavCommand::Bottom, Some(c)) => {
                self.row = (c.max(1) as usize - 1).min(len.saturating_sub(1));
                return;
            }
            (other, _) => other,
        };
        self.row = apply(self.row, len, cmd);
    }

    pub fn move_cols(&mut self, cols: usize, delta: i64, count: Option<u32>) {
        let n = count.unwrap_or(1) as i64;
        self.col = apply(self.col, cols, NavCommand::Move(delta * n));
    }

    pub fn to_row(&mut self, row: usize, len: usize) {
        self.row = row.min(len.saturating_sub(1));
    }

    pub fn clamp(&mut self, len: usize, cols: usize) {
        self.row = self.row.min(len.saturating_sub(1));
        self.col = self.col.min(cols.saturating_sub(1));
    }
}

/// The rows a yank covers: anchor..=cursor in visual mode, the cursor
/// row alone otherwise. Returned as a half-open range.
pub fn selection(mode: &Mode, cursor: &Cursor) -> Range<usize> {
    match mode {
        Mode::Normal => cursor.row..cursor.row + 1,
        Mode::Visual { anchor } => {
            let (a, b) = (cursor.row.min(*anchor), cursor.row.max(*anchor));
            a..b + 1
        }
    }
}

/// The visible index whose node has `path`, or `fallback` clamped.
pub fn restore_by_path(
    visible: &[u32],
    snapshot: &Snapshot,
    plan: &ColumnPlan,
    path: &[Option<String>],
    fallback: usize,
) -> usize {
    visible
        .iter()
        .position(|&r| path_of(snapshot, plan, r as usize) == path)
        .unwrap_or_else(|| fallback.min(visible.len().saturating_sub(1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_shell::vimnav::NavCommand;

    #[test]
    fn row_motion_is_counted_and_clamped() {
        let mut c = Cursor { row: 0, col: 0 };
        c.move_rows(10, NavCommand::Move(1), Some(5));
        assert_eq!(c.row, 5);
        c.move_rows(10, NavCommand::Move(1), Some(50));
        assert_eq!(c.row, 9, "clamped, no wrap");
        c.move_rows(10, NavCommand::Top, None);
        assert_eq!(c.row, 0);
        c.move_rows(10, NavCommand::Bottom, Some(3));
        assert_eq!(c.row, 2, "a counted G goes to that row (1-based)");
        c.move_rows(10, NavCommand::Bottom, None);
        assert_eq!(c.row, 9);
        c.move_rows(0, NavCommand::Move(1), None);
        assert_eq!(c.row, 0, "empty list");
    }

    #[test]
    fn column_motion_is_counted_and_clamped() {
        let mut c = Cursor { row: 0, col: 0 };
        c.move_cols(5, 1, Some(3));
        assert_eq!(c.col, 3);
        c.move_cols(5, 1, Some(9));
        assert_eq!(c.col, 4);
        c.move_cols(5, -1, None);
        assert_eq!(c.col, 3);
        c.clamp(1, 2);
        assert_eq!((c.row, c.col), (0, 1));
    }

    #[test]
    fn a_visual_selection_spans_anchor_to_cursor_either_way() {
        let c = Cursor { row: 2, col: 0 };
        assert_eq!(selection(&Mode::Visual { anchor: 5 }, &c), 2..6);
        assert_eq!(selection(&Mode::Visual { anchor: 0 }, &c), 0..3);
        assert_eq!(selection(&Mode::Normal, &c), 2..3);
    }

    #[test]
    fn the_cursor_returns_to_the_same_node_after_a_requery_or_the_clamped_index() {
        use crate::core::plan::ColumnPlan;
        use geode_core::attribution::{Attribution, ScopeSemantics};
        use geode_core::config::{LayerDoc, merge_docs};
        use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
        use geode_core::view::ViewSpec;
        let dim = |n: &str| ColumnMeta {
            name: n.into(),
            attribution_by_depth: vec![Attribution::Additive; 3],
            scope_semantics: ScopeSemantics::Direct,
        };
        let snap = Snapshot::for_tests(
            vec![
                (
                    dim("lhu"),
                    TestColumn::Str(vec![None, Some("L2"), Some("L1")]),
                ),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 1])),
            ],
            1,
        );
        let doc = merge_docs(
            "views",
            &[LayerDoc::builtin("views", "[t]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n").unwrap()],
        );
        let view = ViewSpec::from_doc(&doc).0.remove(0);
        let plan = ColumnPlan::build(&view, snap.grouping(), &snap);
        let visible = vec![0u32, 1, 2];
        assert_eq!(
            restore_by_path(&visible, &snap, &plan, &[Some("L1".into())], 0),
            2
        );
        assert_eq!(
            restore_by_path(&visible, &snap, &plan, &[Some("GONE".into())], 7),
            2,
            "fallback clamped"
        );
        assert_eq!(
            restore_by_path(&visible, &snap, &plan, &[], 1),
            0,
            "the root's path is empty"
        );
    }
}
