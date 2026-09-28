//! A visible-row and column cursor driven by the shell's `vimnav` commands
//! and the input engine's count.

use crate::core::expansion::path_of;
use crate::core::plan::ColumnPlan;
use geode_core::snapshot::Snapshot;
use geode_shell::vimnav::{NavCommand, apply, apply_clamped};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
}

impl Cursor {
    /// Move the row by `cmd`, applying the count before navigation. With `wrap`,
    /// an effective step of ±1 wraps at the ends; larger steps clamp. The tile
    /// enables wrapping in normal mode and disables it during selection, where
    /// crossing an end would invert the selection relative to its anchor.
    /// A counted `G` addresses a one-based row; `1j` behaves like bare `j`.
    pub fn move_rows(&mut self, len: usize, cmd: NavCommand, count: Option<u32>, wrap: bool) {
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
        self.row = if wrap {
            apply(self.row, len, cmd)
        } else {
            apply_clamped(self.row, len, cmd)
        };
    }

    /// Move horizontally by the counted delta, clamping at either end.
    pub fn move_cols(&mut self, cols: usize, delta: i64, count: Option<u32>) {
        let n = count.unwrap_or(1) as i64;
        self.col = apply_clamped(self.col, cols, NavCommand::Move(delta * n));
    }

    pub fn to_row(&mut self, row: usize, len: usize) {
        self.row = row.min(len.saturating_sub(1));
    }

    pub fn clamp(&mut self, len: usize, cols: usize) {
        self.row = self.row.min(len.saturating_sub(1));
        self.col = self.col.min(cols.saturating_sub(1));
    }
}

/// The visible index whose node has `path`, or `None` when no visible
/// row matches, including an empty `visible` list.
///
/// Search outward from `near`, the last known position, so a small movement
/// after reflattening needs few path comparisons. Check depth before
/// constructing a path to avoid allocations for rows that cannot match.
/// This runs on the UI thread for cursor and selection-anchor restoration;
/// a missing or distant match can still require scanning the entire list.
pub fn find_by_path(
    visible: &[u32],
    snapshot: &Snapshot,
    plan: &ColumnPlan,
    path: &[Option<String>],
    near: usize,
) -> Option<usize> {
    let len = visible.len();
    if len == 0 {
        return None;
    }
    let tree = snapshot.tree();
    let depth = path.len();
    let matches = |i: usize| -> bool {
        let row = visible[i] as usize;
        tree.depth(row) == depth && path_of(snapshot, plan, row) == path
    };
    let start = near.min(len - 1);
    if matches(start) {
        return Some(start);
    }
    let (mut lo, mut hi) = (start, start);
    loop {
        let can_dec = lo > 0;
        let can_inc = hi + 1 < len;
        if !can_dec && !can_inc {
            break;
        }
        if can_dec {
            lo -= 1;
            if matches(lo) {
                return Some(lo);
            }
        }
        if can_inc {
            hi += 1;
            if matches(hi) {
                return Some(hi);
            }
        }
    }
    None
}

/// `find_by_path`, falling back to `fallback` clamped.
pub fn restore_by_path(
    visible: &[u32],
    snapshot: &Snapshot,
    plan: &ColumnPlan,
    path: &[Option<String>],
    fallback: usize,
) -> usize {
    find_by_path(visible, snapshot, plan, path, fallback)
        .unwrap_or_else(|| fallback.min(visible.len().saturating_sub(1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_shell::vimnav::NavCommand;

    #[test]
    fn row_motion_is_counted_and_clamped_but_a_bare_step_wraps() {
        let mut c = Cursor { row: 0, col: 0 };
        c.move_rows(10, NavCommand::Move(1), Some(5), true);
        assert_eq!(c.row, 5);
        c.move_rows(10, NavCommand::Move(1), Some(50), true);
        assert_eq!(c.row, 9, "a counted step clamps, no wrap");
        c.move_rows(10, NavCommand::Move(1), None, true);
        assert_eq!(c.row, 0, "a bare j at the bottom wraps to the top");
        c.move_rows(10, NavCommand::Move(-1), None, true);
        assert_eq!(c.row, 9, "and a bare k at the top wraps to the bottom");
        c.move_rows(10, NavCommand::Move(1), Some(1), true);
        assert_eq!(
            c.row, 0,
            "1j is j: the count is multiplied in, not special-cased"
        );
        c.move_rows(10, NavCommand::Top, None, true);
        assert_eq!(c.row, 0);
        c.move_rows(10, NavCommand::Bottom, Some(3), true);
        assert_eq!(c.row, 2, "a counted G goes to that row (1-based)");
        c.move_rows(10, NavCommand::Bottom, None, true);
        assert_eq!(c.row, 9);
        c.move_rows(0, NavCommand::Move(1), None, true);
        assert_eq!(c.row, 0, "empty list");
    }

    #[test]
    fn visual_mode_clamps_a_bare_step() {
        // `wrap = false` is what the tile passes while a grid selection
        // is live: a wrap would put the cursor above the anchor and
        // invert the selection.
        let mut c = Cursor { row: 9, col: 0 };
        c.move_rows(10, NavCommand::Move(1), None, false);
        assert_eq!(c.row, 9);
        c.row = 0;
        c.move_rows(10, NavCommand::Move(-1), None, false);
        assert_eq!(c.row, 0);
    }

    #[test]
    fn column_motion_is_counted_and_clamped() {
        let mut c = Cursor { row: 0, col: 0 };
        c.move_cols(5, 1, Some(3));
        assert_eq!(c.col, 3);
        c.move_cols(5, 1, Some(9));
        assert_eq!(c.col, 4);
        c.move_cols(5, 1, None);
        assert_eq!(c.col, 4, "l at the last column stays: columns never wrap");
        c.move_cols(5, -1, None);
        assert_eq!(c.col, 3);
        c.clamp(1, 2);
        assert_eq!((c.row, c.col), (0, 1));
    }

    /// Shown rows: root, L1, L1/SPX. Shared by the `find_by_path` /
    /// `restore_by_path` tests below.
    fn fixture() -> (
        geode_core::snapshot::Snapshot,
        crate::core::plan::ColumnPlan,
        Vec<u32>,
    ) {
        use crate::core::plan::ColumnPlan;
        use geode_core::attribution::{Attribution, ScopeSemantics};
        use geode_core::config::{LayerDoc, merge_docs};
        use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
        use geode_core::view::ViewSpec;

        let dim = |n: &str| ColumnMeta {
            name: n.into(),
            attribution_by_depth: vec![Attribution::Additive; 3],
            scope_semantics: ScopeSemantics::Direct,
            summable: false,
            mixed_flag: None,
        };
        let snap = Snapshot::for_tests(
            vec![
                (
                    dim("lhu"),
                    TestColumn::Str(vec![None, Some("L1"), Some("L1")]),
                ),
                (
                    dim("underlying_ref"),
                    TestColumn::Str(vec![None, None, Some("SPX")]),
                ),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 2])),
            ],
            2,
        );
        let doc = merge_docs(
            "views",
            &[LayerDoc::builtin(
                "views",
                "[t]\ndataset = \"d\"\ngrouping = [\"lhu\", \"underlying_ref\"]\n",
            )
            .unwrap()],
        );
        let view = ViewSpec::from_doc(&doc).0.remove(0);
        let plan = ColumnPlan::build(&view, snap.grouping(), &snap);
        (snap, plan, vec![0, 1, 2])
    }

    #[test]
    fn find_by_path_is_exact_and_restore_falls_back() {
        // fixture: shown rows [root, L1, L1/SPX]; path ["L2"] absent.
        let (snap, plan, shown) = fixture();
        let l1 = path_of(&snap, &plan, shown[1] as usize);
        assert_eq!(find_by_path(&shown, &snap, &plan, &l1, 0), Some(1));
        let missing = vec![Some("L2".to_string())];
        assert_eq!(find_by_path(&shown, &snap, &plan, &missing, 1), None);
        assert_eq!(restore_by_path(&shown, &snap, &plan, &missing, 1), 1);
        assert_eq!(find_by_path(&[], &snap, &plan, &l1, 0), None);
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
            summable: false,
            mixed_flag: None,
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

    /// A depth-1 target follows 500 rows of mixed depth-1 and depth-2 filler.
    /// Restoration must find its path even when the fallback is at the root.
    /// Depth-1 filler exercises path mismatches; depth-2 filler cannot match
    /// the target's depth.
    #[test]
    fn restore_by_path_finds_the_row_when_many_precede_it_at_other_depths() {
        use crate::core::plan::ColumnPlan;
        use geode_core::attribution::{Attribution, ScopeSemantics};
        use geode_core::config::{LayerDoc, merge_docs};
        use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
        use geode_core::view::ViewSpec;

        let dim = |n: &str| ColumnMeta {
            name: n.into(),
            attribution_by_depth: vec![Attribution::Additive; 3],
            scope_semantics: ScopeSemantics::Direct,
            summable: false,
            mixed_flag: None,
        };
        let mut lhu: Vec<Option<String>> = vec![None]; // root
        let mut und: Vec<Option<String>> = vec![None];
        let mut pos: Vec<Option<String>> = vec![None];
        let mut depth: Vec<i32> = vec![0];
        const N: usize = 250;
        for i in 0..N {
            // depth-1 filler
            lhu.push(Some(format!("L{i}")));
            und.push(None);
            pos.push(None);
            depth.push(1);
        }
        for i in 0..N {
            // depth-2 filler, a *different* depth than the target below
            lhu.push(Some(format!("L{i}")));
            und.push(Some(format!("U{i}")));
            pos.push(None);
            depth.push(2);
        }
        // The target: one more depth-1 row, at the very end — 2*N = 500
        // rows of mixed-depth filler precede it.
        lhu.push(Some("TARGET".into()));
        und.push(None);
        pos.push(None);
        depth.push(1);

        let snap = Snapshot::for_tests(
            vec![
                (dim("lhu"), TestColumn::Dict(lhu)),
                (dim("underlying_ref"), TestColumn::Dict(und)),
                (dim("position_ref"), TestColumn::Dict(pos)),
                (dim("row_depth"), TestColumn::I32(depth)),
            ],
            3,
        );
        let doc = merge_docs(
            "views",
            &[LayerDoc::builtin(
                "views",
                "[t]\ndataset = \"d\"\ngrouping = [\"lhu\", \"underlying_ref\", \"position_ref\"]\n",
            )
            .unwrap()],
        );
        let view = ViewSpec::from_doc(&doc).0.remove(0);
        let plan = ColumnPlan::build(&view, snap.grouping(), &snap);
        let visible: Vec<u32> = (0..snap.rows() as u32).collect();
        let target_row = snap.rows() - 1;

        assert_eq!(
            restore_by_path(&visible, &snap, &plan, &[Some("TARGET".to_string())], 0),
            target_row,
            "the depth-1 target is found correctly past 500 rows of \
             mixed-depth filler, with `fallback` (0) nowhere near it"
        );
    }
}
