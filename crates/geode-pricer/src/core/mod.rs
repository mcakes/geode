//! The sheet's pure core (line-pricer spec §6): the row model, the one
//! edit door with its inverses, the shorthand grammar both ways, the
//! package templates, the column vocabulary, the views doc and the
//! storage row shape, and the entry bar's completer. No gpui type appears
//! here except `SharedString`, which `complete` prepares so the entry
//! bar's list paints without formatting.

pub mod cell;
pub mod clip;
pub mod columns;
pub mod commands;
pub mod complete;
pub mod edit;
pub mod entry;
pub mod package;
pub mod sheet;
pub mod shorthand;
pub mod storage;
pub mod template;
pub mod tree;
pub mod undo;
pub mod views;

pub use cell::{CellEditor, READ_ONLY};
pub use columns::{
    Applies, COLUMNS, CellState, CellText, ColumnDef, ColumnKind, cell_text, column,
};
pub use complete::{Completion, Inputs, MAX_ROWS, Slot, Suggestion, slot_at};
pub use edit::{Edit, EditError, Undo};
pub use sheet::{
    Delivered, LineId, LineSpec, LineState, OwnShifts, Place, Refresh, RowKind, RowRecord, RowSpec,
    Sheet,
};
pub use shorthand::{ParseError, parse, render_expiry, render_line, render_package, render_strike};
pub use storage::{
    LINE_AXIS, PRICER_SHEETS_DATASET, PRICER_SHEETS_DECLARATION, SHEET_KEY, from_rows, to_rows,
};
pub use template::{
    BUILTIN_TEMPLATES, LegSpec, MAX_TEMPLATE_NAME, PRICER_TEMPLATES_DOC, Template, TemplateDef,
    TemplateSet, check_name,
};
pub use tree::{Expansion, visible_rows};
pub use undo::{UNDO_DEPTH, UndoStack};
pub use views::{
    BUILTIN_VIEWS, ColumnPlan, PRICER_VIEWS_DOC, PlannedColumn, PricerView, ViewColumn, Views,
};
