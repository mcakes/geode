//! The panel's pure core (market-data spec §8.2/§8.4): no element, entity
//! or window, tested without one.
//!
//! The division of labour is the one PHILOSOPHY §6 asks for. `spec`
//! declares what a panel is; `matrix` turns one delivered `Snapshot` plus
//! one `Draft` into prepared strings ONCE per delivery or edit, so a frame
//! clones `SharedString`s and formats nothing; `draft` is the unsent work,
//! keyed by grid cell and resolved across generations by row and column
//! *label*, because an index means nothing once a new document arrives.

pub mod cursor;
pub mod datefield;
pub mod draft;
pub mod matrix;
pub mod menu;
pub mod nudge;
pub mod spec;
#[cfg(test)]
pub(crate) mod test_fixtures;

pub use cursor::Cursor;
pub use datefield::{DateField, Segment, SegmentText};
pub use draft::{Draft, DraftBadge, DraftState, UpdatePolicy, attr_text, parse_attr, parse_cell};
pub use matrix::{Cell, CellKind, MatrixModel, RowModel, cell_text};
pub use nudge::nudge_text;
pub use spec::{CVI, Columns, DIVIDEND, KindAction, PanelSpec, STATUSES};
