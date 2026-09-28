//! Market-data panel definitions: the declarative half of a panel. A panel
//! names the document dataset it shows, the registered document kind it
//! edits, how the grid lays that document out, and which registered kind
//! actions it offers. What a kind action does stays code, registered by id
//! in the composition root.

use crate::config::{Diagnostic, MergedDoc, Severity};
use crate::schema::ColumnType;
use crate::view::ColumnFormat;
use std::sync::Arc;

#[cfg(test)]
mod tests;

mod read;

pub use read::read_panels;

/// The layered document panels are read from.
pub const PANELS_DOC: &str = "panels";

/// A header attribute's source column, display label, and edit type. The
/// declared type belongs here because snapshot metadata does not carry the
/// dataset's schema type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderAttr {
    pub column: String,
    pub label: String,
    pub ty: ColumnType,
}

/// A kind-specific action a panel may offer in its menu, palette and keymap.
/// Registered code, so its id and title are static; `built: false` disables
/// the menu row with a reason and the tile answers "not built yet".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindAction {
    pub id: &'static str,
    pub title: &'static str,
    pub built: bool,
}

/// A value constant across a row-axis slice, such as CVI's forward per
/// term. The long document repeats it on every node row; the grid paints
/// it before the ladder using its own format. Row bumps skip these columns.
/// Slice values are read and uploaded as `f64`; their editors use the
/// panel's `value_type`, which must therefore be compatible.
#[derive(Debug, Clone, PartialEq)]
pub struct SliceValue {
    pub column: String,
    /// The short column header. Must not collide with any label the column
    /// axis can produce, or another slice value's: a draft resolves an edit
    /// by `(row label, column label)`, so two columns one name would send an
    /// edit to the wrong one.
    pub label: String,
    pub format: ColumnFormat,
}

/// Who names a new row: the trader (`Typed`, the row-label editor opens on
/// insert, parsed as the given type) or the panel (`Minted`, `new-<n>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowIdentity {
    Typed(ColumnType),
    Minted,
}

/// Whether to paint a row-label column. A hidden label still identifies
/// draft rows across restoration and rebase; only minted identities may
/// hide it, since a typed identity is named in that column.
/// With a hidden label, table column 0 becomes the first value, and
/// search and copy operate on the painted cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowLabel {
    Shown,
    Hidden,
}

/// The axis down the side: the column a row's label comes from, who
/// chooses it, and whether it is painted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowAxis {
    pub column: String,
    pub identity: RowIdentity,
    pub label: RowLabel,
}

impl RowAxis {
    /// Whether the table carries a row-label column.
    pub fn shown(&self) -> bool {
        self.label == RowLabel::Shown
    }
}

/// One flat column: what it reads, how it paints, how it is edited, and
/// whether an inserted row must fill it. `choices` is shared rather than
/// copied because every model build hands it to each cell kind.
#[derive(Debug, Clone, PartialEq)]
pub struct ValueColumn {
    pub column: String,
    pub label: String,
    pub ty: ColumnType,
    pub format: ColumnFormat,
    pub choices: Option<Arc<[String]>>,
    pub required: bool,
}

/// How the columns across the top are chosen.
#[derive(Debug, Clone, PartialEq)]
pub enum Columns {
    /// Pivot: one column per distinct value of this axis, in document order;
    /// every cell is the document's one unnamed value column.
    Axis(String),
    /// Flat: one row per document row, one column per listed value column.
    Values(Vec<ValueColumn>),
}

/// One panel: the dataset it reads, the document kind it edits, how it lays
/// a document out, what its header shows, and how its numbers are formatted.
#[derive(Debug, Clone, PartialEq)]
pub struct PanelSpec {
    /// The tile kind and session `module` name: the panel's name in `panels`.
    pub kind: String,
    pub title: String,
    pub dataset: String,
    /// The registered document kind used to serialize `:upload`.
    pub document: String,
    pub rows: RowAxis,
    pub columns: Columns,
    /// Document-level attributes shown in the header, in this order.
    pub header: Vec<HeaderAttr>,
    /// Per-slice values painted ahead of the ladder (`Columns::Axis` only).
    pub slice_values: Vec<SliceValue>,
    pub format: ColumnFormat,
    /// Declared numeric edit and upload type for a pivot's ladder, and the
    /// fallback type of a flat column index past the declared columns.
    /// Snapshot metadata does not expose schema types, and the runtime array
    /// representation alone cannot decide whether an editor may accept
    /// fractional input.
    pub value_type: ColumnType,
    /// Kind-specific actions listed under this panel's title and registered
    /// for palette and keymap dispatch, in menu order.
    pub actions: Vec<KindAction>,
}

impl PanelSpec {
    /// Whether the panel names this row axis, pivot axis, flat value, header
    /// attribute, or slice value. Key extraction excludes these columns from
    /// the prefix before the row axis.
    pub fn names(&self, column: &str) -> bool {
        self.rows.column == column
            || matches!(&self.columns, Columns::Axis(a) if a == column)
            || self.value_column(column).is_some()
            || self.header.iter().any(|h| h.column == column)
            || self.slice_value(column).is_some()
    }

    /// The slice value painted from `column`, if any — how the pivot tells a
    /// named per-slice value from a second value column it must refuse (both
    /// arrive `DeterminedNonAdditive`).
    pub fn slice_value(&self, column: &str) -> Option<&SliceValue> {
        self.slice_values.iter().find(|s| s.column == column)
    }

    /// The flat column painted from `column`, if the layout is flat and lists it.
    pub fn value_column(&self, column: &str) -> Option<&ValueColumn> {
        self.flat_columns().iter().find(|c| c.column == column)
    }

    /// The flat layout's columns in paint order; empty under a pivot.
    pub fn flat_columns(&self) -> &[ValueColumn] {
        match &self.columns {
            Columns::Values(cols) => cols,
            Columns::Axis(_) => &[],
        }
    }
}

/// Every kind action this build can offer, by id. A panel lists ids; the
/// reader resolves each here and refuses an id nobody registered. Adding a
/// verb is code, registered by the composition root.
#[derive(Debug, Clone, Default)]
pub struct KindActionRegistry {
    actions: Vec<KindAction>,
}

impl KindActionRegistry {
    /// Register `action`. A second registration of one id is refused: two
    /// verbs behind one id would make a panel's menu row do either.
    pub fn register(&mut self, action: KindAction) -> Result<(), String> {
        if self.get(action.id).is_some() {
            return Err(format!("kind action '{}' is registered twice", action.id));
        }
        self.actions.push(action);
        Ok(())
    }

    /// The action registered under `id`, or `None` when nobody registered it.
    pub fn get(&self, id: &str) -> Option<KindAction> {
        self.actions.iter().find(|a| a.id == id).copied()
    }

    /// Registered ids in registration order, for diagnostics.
    pub fn ids(&self) -> Vec<&'static str> {
        self.actions.iter().map(|a| a.id).collect()
    }
}

/// An Error refusing panel `name` at `panels.<name>[.<suffix>]`, attributed
/// to the layer that supplied it. A refused panel never becomes a tile kind.
pub fn refusal(doc: &MergedDoc, name: &str, suffix: &str, message: &str) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        layer: doc.provenance.get(name).copied(),
        file: None,
        message: format!("panel '{name}': {message}; the panel is refused"),
        path: Some(if suffix.is_empty() {
            format!("{PANELS_DOC}.{name}")
        } else {
            format!("{PANELS_DOC}.{name}.{suffix}")
        }),
    }
}
