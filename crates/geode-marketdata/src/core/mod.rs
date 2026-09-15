//! The panel's pure core (market-data spec §8.2/§8.4): no element, entity
//! or window, tested without one.
//!
//! The division of labour is the one PHILOSOPHY §6 asks for. `spec`
//! declares what a panel is; `matrix` turns one delivered `Snapshot` plus
//! one `Draft` into prepared strings ONCE per delivery or edit, so a frame
//! clones `SharedString`s and formats nothing; `draft` is the unsent work,
//! keyed by grid cell and resolved across generations by row and column
//! *label*, because an index means nothing once a new document arrives.

pub mod draft;
pub mod matrix;
pub mod spec;

pub use draft::{Draft, DraftBadge, DraftState, attr_text, parse_attr, parse_cell};
pub use matrix::{Cell, MatrixModel, RowModel};
pub use spec::{CVI, Columns, PanelSpec};
