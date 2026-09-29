//! Display order is separate from search text. The table can show every loaded
//! parent and leaf while labels and normalization are prepared on a worker.
use crate::core::{
    expansion::path_of,
    flatten::{SortSpec, flatten},
    plan::ColumnPlan,
};
use geode_core::{expansion::Expansion, snapshot::Snapshot};
use geode_shell::fuzzyfind::{FindIndex, FindLabel, FindTree};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(crate) struct Prepared {
    pub snapshot: Arc<Snapshot>,
    pub plan: ColumnPlan,
    pub sort: Option<SortSpec>,
    pub rows: Arc<[u32]>,
    pub index: Arc<FindIndex>,
    tree: Arc<FindTree>,
    pub complete: bool,
}

impl Prepared {
    pub fn matches(
        &self,
        snapshot: &Arc<Snapshot>,
        plan: &ColumnPlan,
        sort: Option<&SortSpec>,
    ) -> bool {
        Arc::ptr_eq(&self.snapshot, snapshot) && self.plan == *plan && self.sort.as_ref() == sort
    }
}

fn label(snapshot: &Arc<Snapshot>, plan: &ColumnPlan, row: u32) -> FindLabel {
    let label = plan.tree_text(snapshot, row as usize).unwrap_or("");
    let mut path = path_of(snapshot, plan, row as usize);
    let identity = if snapshot.has_depth_column() {
        format!("{:?}:{path:?}", snapshot.grouping())
    } else {
        format!("{:p}:{row}", Arc::as_ptr(snapshot))
    };
    path.pop();
    let context = path
        .iter()
        .map(|part| part.as_deref().unwrap_or("—"))
        .collect::<Vec<_>>()
        .join(" › ");
    FindLabel::new(identity, label, context)
}

/// Prepare only row indices up front. No per-row labels, paths, normalized text,
/// or callbacks are allocated until visible or requested by the search worker.
pub(crate) fn display(
    snapshot: Arc<Snapshot>,
    plan: ColumnPlan,
    sort: Option<SortSpec>,
    shown: &[u32],
    complete: bool,
) -> Prepared {
    let rows = if sort.is_none() {
        snapshot.tree().depth_first_rows()
    } else if shown.len() == snapshot.rows() {
        // The existing table already has the full sorted order.
        Arc::from(shown)
    } else {
        let mut rows = Vec::with_capacity(snapshot.rows());
        let mut expansion = Expansion::default();
        expansion.open_all();
        flatten(&snapshot, &plan, &expansion, sort.as_ref(), &mut rows);
        Arc::from(rows)
    };
    let depths = (snapshot.clone(), rows.clone());
    let branches = depths.clone();
    let tree = Arc::new(FindTree::deferred(
        rows.len(),
        move |row| depths.0.tree().depth(depths.1[row] as usize),
        move |row| branches.0.tree().has_children(branches.1[row] as usize),
    ));
    let source = (snapshot.clone(), plan.clone(), rows.clone());
    let index = Arc::new(
        FindIndex::deferred(rows.len(), move |row| {
            label(&source.0, &source.1, source.2[row])
        })
        .with_tree(tree.clone()),
    );
    Prepared {
        snapshot,
        plan,
        sort,
        rows,
        index,
        tree,
        complete,
    }
}

pub(crate) fn prepare(display: &Prepared, cancelled: &AtomicBool) -> Option<Prepared> {
    display.tree.prepare();
    let mut labels = Vec::with_capacity(display.rows.len());
    for &row in display.rows.iter() {
        if cancelled.load(Ordering::Relaxed) {
            return None;
        }
        labels.push(label(&display.snapshot, &display.plan, row));
    }
    let index = Arc::new(FindIndex::new(labels).with_tree(display.tree.clone()));
    if cancelled.load(Ordering::Relaxed) {
        return None;
    }
    Some(Prepared {
        snapshot: display.snapshot.clone(),
        plan: display.plan.clone(),
        sort: display.sort.clone(),
        rows: display.rows.clone(),
        index,
        tree: display.tree.clone(),
        complete: display.complete,
    })
}
