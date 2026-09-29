//! Immutable, thread-safe search data. Preparing an index is deliberately separate
//! from installing a UI session, so large tiles can build once on a worker.
use super::{FindTree, Rows, SearchText};
use gpui::SharedString;
use std::borrow::Cow;
use std::sync::Arc;

/// Stable identity, displayed label, and ancestor context for one search row.
#[derive(Clone)]
pub struct FindLabel {
    pub(super) id: SharedString,
    pub(super) label: String,
    pub(super) path: String,
}

impl FindLabel {
    pub fn new(
        id: impl Into<SharedString>,
        label: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            path: path.into(),
        }
    }
}

/// A reusable dataset and its normalized search text. Build on a background
/// executor, then install with `FuzzyFind::replace_index` without copying the dataset.
pub struct FindIndex {
    labels: Labels,
    pub(super) tree: Option<Arc<FindTree>>,
    pub(super) ready: bool,
    pub(super) search: Arc<[SearchText]>,
    pub(super) natural: Rows,
}

impl FindIndex {
    pub fn new(labels: Vec<FindLabel>) -> Self {
        let search = labels
            .iter()
            .map(|label| SearchText::new(&label.label, &label.path))
            .collect();
        let natural = Rows::All(labels.len());
        Self {
            labels: Labels::Ready(labels),
            tree: None,
            ready: true,
            search,
            natural,
        }
    }
}

enum Labels {
    Ready(Vec<FindLabel>),
    Deferred(Arc<dyn Fn(usize) -> FindLabel + Send + Sync>),
}

impl FindIndex {
    /// Display every source row immediately, preparing labels only for visible
    /// rows. Install a fully prepared index later to enable query ranking.
    pub fn deferred(
        count: usize,
        label: impl Fn(usize) -> FindLabel + Send + Sync + 'static,
    ) -> Self {
        Self {
            labels: Labels::Deferred(Arc::new(label)),
            tree: None,
            ready: false,
            search: Arc::from([]),
            natural: Rows::All(count),
        }
    }

    pub fn with_tree(mut self, tree: Arc<FindTree>) -> Self {
        assert_eq!(tree.len(), self.natural.len());
        self.tree = Some(tree);
        self
    }

    pub(super) fn label(&self, row: usize) -> Cow<'_, FindLabel> {
        match &self.labels {
            Labels::Ready(labels) => Cow::Borrowed(&labels[row]),
            Labels::Deferred(label) => Cow::Owned(label(row)),
        }
    }
}
