//! One history request read from the cache on a pool worker. The service
//! decides what the source must be asked first; by the time this runs, the
//! rows it reads are what the request is answered with.

use std::sync::Arc;

use duckdb::Connection;
use geode_core::query::{HistoryParams, HistoryTable};
use geode_core::schema::SchemaSpec;

use crate::store::StoreError;

#[derive(Debug)]
pub struct HistoryRead {
    schema: Arc<SchemaSpec>,
    params: HistoryParams,
}

impl HistoryRead {
    pub(crate) fn new(schema: Arc<SchemaSpec>, params: HistoryParams) -> Self {
        Self { schema, params }
    }

    pub fn dataset(&self) -> &str {
        &self.params.dataset
    }

    /// Refuses a dataset that is undeclared or not of the history family:
    /// its tables would not have the history layout the read assumes.
    pub(crate) fn run(&self, conn: &Connection) -> Result<HistoryTable, StoreError> {
        let ds = self
            .schema
            .dataset(&self.params.dataset)
            .filter(|d| d.is_history())
            .ok_or_else(|| {
                StoreError::History(format!(
                    "'{}' is not a declared history dataset",
                    self.params.dataset
                ))
            })?;
        crate::store::history::read_history(conn, ds, &self.params.keys, self.params.span)
    }
}
