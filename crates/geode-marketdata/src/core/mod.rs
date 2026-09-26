//! Market-data model preparation and draft transitions, without entities
//! or windows. `spec` defines compiled panels; `matrix` prepares and patches
//! display cells; `draft` stores edits with the labels needed for rebase;
//! `upload` assembles typed rows and compares delivered echoes.

pub mod cursor;
pub mod draft;
pub mod matrix;
pub mod menu;
pub mod spec;
#[cfg(test)]
pub(crate) mod test_fixtures;
pub mod upload;

pub use cursor::Cursor;
pub use draft::{Draft, DraftBadge, DraftState, UpdatePolicy, attr_text, parse_attr, parse_cell};
/// Shared numeric text nudging used by market-data and pricing editors.
pub use geode_core::nudge::nudge_text;
pub use geode_widgets::datefield::{
    DateTimeField, FieldKey, Precision, Segment, SegmentPaint, SegmentText, route,
};
pub use matrix::{Cell, CellKind, MatrixModel, RowModel, cell_text};
pub use spec::{CVI, Columns, DIVIDEND, KindAction, PanelSpec, STATUSES};
