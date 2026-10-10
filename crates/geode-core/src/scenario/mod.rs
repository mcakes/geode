//! Scenario tile shapes: the declarative half of a scenario tile kind. A
//! shape names the axis dataset it reads, which axis runs down the side,
//! what runs across (one or two axes, or a list of measures), the measure
//! a matrix paints, an optional diff against a base row, and the paint a
//! tile starts with. Reading is in two halves like `panel`: `read_scenarios`
//! judges each entry alone; `load_scenarios` checks it against the schema
//! and fills in what only the schema knows (axis kinds and orders, `_usd`
//! twins). Every problem refuses the entry: a shape that disagrees with its
//! dataset would paint a plausible wrong grid.

use crate::config::{Diagnostic, MergedDoc, Severity};
use crate::schema::ColumnType;
use crate::schema::axis::{AxisKind, AxisOrder};
use crate::view::ColumnFormat;

#[cfg(test)]
mod tests;

mod check;
mod read;

pub use check::load_scenarios;
pub use read::read_scenarios;

/// The layered document shapes are read from.
pub const SCENARIOS_DOC: &str = "scenarios";

/// A measure a shape paints: as a matrix's one value or as one column of
/// a ladder. `usd_twin` is filled by `load_scenarios` when the dataset
/// declares `<column>_usd` as a measure; `read_scenarios` leaves it `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct MeasureColumn {
    pub column: String,
    /// The header text; defaults to the column name.
    pub label: String,
    pub format: ColumnFormat,
    /// Text under the header (`bp`, `per vol pt`).
    pub unit: Option<String>,
    pub usd_twin: Option<String>,
}

/// What runs across the top.
#[derive(Debug, Clone, PartialEq)]
pub enum Columns {
    /// One or two axes, outer first; two make a two-level header.
    Axes(Vec<String>),
    /// One column per listed measure.
    Measures(Vec<MeasureColumn>),
}

/// The value of the `rows` axis the diff subtracts: a number for a numeric
/// axis, text otherwise. `load_scenarios` refuses the wrong one.
#[derive(Debug, Clone, PartialEq)]
pub enum DiffBase {
    Number(f64),
    Text(String),
}

/// Paint the listed measures as value minus the base row's value; the base
/// row stays absolute and is highlighted.
#[derive(Debug, Clone, PartialEq)]
pub struct Diff {
    pub base: DiffBase,
    pub measures: Vec<String>,
}

/// The paint a tile starts with; `p` cycles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paint {
    Heat,
    Plain,
}

impl Paint {
    pub fn parse(s: &str) -> Option<Paint> {
        match s {
            "heat" => Some(Paint::Heat),
            "plain" => Some(Paint::Plain),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Paint::Heat => "heat",
            Paint::Plain => "plain",
        }
    }
}

/// One dataset axis as the pivot needs it: how it sums, how it sorts and
/// how its labels parse. Filled by `load_scenarios` in dataset `axes` order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AxisInfo {
    pub name: String,
    pub kind: AxisKind,
    pub order: AxisOrder,
    pub ty: ColumnType,
}

impl AxisInfo {
    /// Labels parse as numbers and sort numerically.
    pub fn is_numeric(&self) -> bool {
        matches!(self.ty, ColumnType::F64 | ColumnType::I64)
    }
}

/// One shape: the tile kind it defines and everything the pivot and the
/// tile need. `axes` and the twins are empty out of `read_scenarios`.
#[derive(Debug, Clone, PartialEq)]
pub struct ShapeSpec {
    /// The tile kind and session `module` name: the entry's name.
    pub kind: String,
    pub title: String,
    pub dataset: String,
    pub rows: String,
    pub columns: Columns,
    /// The measure a matrix paints; `Some` exactly when `columns` is `Axes`.
    pub value: Option<MeasureColumn>,
    pub diff: Option<Diff>,
    pub paint: Paint,
    /// The dataset's axes, in its `axes` order, once checked.
    pub axes: Vec<AxisInfo>,
}

impl ShapeSpec {
    pub fn axis(&self, name: &str) -> Option<&AxisInfo> {
        self.axes.iter().find(|a| a.name == name)
    }

    /// The measures painted, in column order: the one value of a matrix or
    /// the ladder's columns.
    pub fn measures(&self) -> Vec<&MeasureColumn> {
        match (&self.columns, &self.value) {
            (Columns::Measures(cols), _) => cols.iter().collect(),
            (Columns::Axes(_), Some(v)) => vec![v],
            (Columns::Axes(_), None) => Vec::new(),
        }
    }

    /// The columns axes, empty for a ladder.
    pub fn column_axes(&self) -> &[String] {
        match &self.columns {
            Columns::Axes(axes) => axes,
            Columns::Measures(_) => &[],
        }
    }

    /// The grouping of the tile's main query: the rows axis, then the
    /// columns axes.
    pub fn grouping(&self) -> Vec<String> {
        let mut g = vec![self.rows.clone()];
        g.extend(self.column_axes().iter().cloned());
        g
    }

    /// Whether a Σ across the rows (one per column) is meaningful: the rows
    /// axis partitions a total. False before `load_scenarios`.
    pub fn sums_rows(&self) -> bool {
        self.axis(&self.rows)
            .is_some_and(|a| a.kind == AxisKind::Bucket)
    }

    /// Whether a Σ across the columns (one per row) is meaningful: every
    /// columns axis partitions a total. Never across measures.
    pub fn sums_columns(&self) -> bool {
        let axes = self.column_axes();
        !axes.is_empty()
            && axes
                .iter()
                .all(|c| self.axis(c).is_some_and(|a| a.kind == AxisKind::Bucket))
    }

    /// The grouping of the second query a matrix issues for its Σ row, when
    /// one is meaningful. A ladder takes its Σ row from the main query's
    /// total row and needs no second query.
    pub fn totals_grouping(&self) -> Option<Vec<String>> {
        match &self.columns {
            Columns::Axes(axes) if self.sums_rows() => Some(axes.clone()),
            _ => None,
        }
    }

    /// Every snapshot column the query must carry: each measure and its
    /// twin, once each, in column order — so the currency toggle needs no
    /// requery.
    pub fn query_measures(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for m in self.measures() {
            for name in std::iter::once(&m.column).chain(m.usd_twin.iter()) {
                if !out.contains(name) {
                    out.push(name.clone());
                }
            }
        }
        out
    }
}

/// An Error refusing shape `name` at `scenarios.<name>[.<suffix>]`,
/// attributed to the layer that supplied it. A refused shape never becomes
/// a tile kind.
pub fn refusal(doc: &MergedDoc, name: &str, suffix: &str, message: &str) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        layer: doc.provenance.get(name).copied(),
        file: None,
        message: format!("scenario shape '{name}': {message}; the shape is refused"),
        path: Some(if suffix.is_empty() {
            format!("{SCENARIOS_DOC}.{name}")
        } else {
            format!("{SCENARIOS_DOC}.{name}.{suffix}")
        }),
    }
}
