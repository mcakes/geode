//! The grouped view a scenario tile queries, derived from its shape. A
//! shape names a dataset and axes, not a view, and the data service answers
//! queries by view name; so the app derives one view per accepted shape and
//! registers it with the service alone (never with the blotter's list).
//! The grouping is the shape's axes in shape order, the columns every
//! painted measure and its twin (so `c` needs no requery), and `currency`
//! rides along ungrouped where the dataset declares it as a dimension, so
//! the snapshot's mixed flag says whether a native sum crosses currencies.

use super::ShapeSpec;
use crate::schema::{ColumnRole, DatasetSpec};
use crate::view::{ViewColumn, ViewSpec};

/// Derived view names: `scenario:<kind>`. The service answers the first
/// view of a name, so the two never share one: at startup a configured
/// view of this name refuses the shape; one configured by a later reload
/// meets a running shape and is dropped from the hand-off with a Warning.
pub const VIEW_PREFIX: &str = "scenario:";

impl ShapeSpec {
    /// The name of the view this shape's tile queries.
    pub fn view_name(&self) -> String {
        format!("{VIEW_PREFIX}{}", self.kind)
    }

    /// The view: `ds` must be the shape's dataset as the schema declares
    /// it (the caller looked it up by `self.dataset`).
    pub fn derive_view(&self, ds: &DatasetSpec) -> ViewSpec {
        let mut columns: Vec<ViewColumn> = self
            .query_measures()
            .into_iter()
            .map(ViewColumn::measure)
            .collect();
        if ds
            .column("currency")
            .is_some_and(|c| matches!(c.role, ColumnRole::Dimension { .. }))
        {
            columns.push(ViewColumn::dimension("currency"));
        }
        ViewSpec {
            name: self.view_name(),
            dataset: self.dataset.clone(),
            columns,
            grouping: self.grouping(),
            ..ViewSpec::default()
        }
    }
}
