//! The cross-document half of the `scenarios` reader (Task 2).
use super::ShapeSpec;
use crate::config::{Diagnostic, MergedDoc};
use crate::schema::SchemaSpec;

pub fn load_scenarios(doc: &MergedDoc, schema: &SchemaSpec) -> (Vec<ShapeSpec>, Vec<Diagnostic>) {
    let _ = schema;
    super::read_scenarios(doc)
}
