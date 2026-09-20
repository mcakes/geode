//! The sheet's pure core (line-pricer spec §6): the row model, the one
//! edit door with its inverses, the shorthand grammar both ways, the
//! package templates, the column vocabulary, the views doc and the
//! storage row shape. No gpui type appears here.

pub mod columns;
pub mod edit;
pub mod sheet;
pub mod shorthand;
pub mod template;

pub use columns::{
    Applies, COLUMNS, CellState, CellText, ColumnDef, ColumnKind, cell_text, column,
};
pub use edit::{Edit, EditError, Undo};
pub use sheet::{
    Delivered, LineId, LineSpec, LineState, OwnShifts, Place, Refresh, RowKind, RowRecord, RowSpec,
    Sheet,
};
pub use shorthand::{ParseError, parse, render_expiry, render_line, render_package, render_strike};
pub use template::{LegSpec, Template};
