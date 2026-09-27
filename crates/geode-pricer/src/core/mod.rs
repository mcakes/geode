//! The sheet's pure model: row identity and structure, edits and inverses, shorthand
//! parsing and rendering, package templates, column formatting, views, and document
//! conversion. This module has no GPUI types or I/O.

pub mod cell;
pub mod clip;
pub mod columns;
pub mod commands;
pub mod edit;
pub mod entry;
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
