//! What a panel is (market-data spec §8.1).
//!
//! A spec is code in slice 1, not config: one panel exists (CVI), and
//! making specs configurable before a second one shows what actually
//! varies would be guessing. A `&'static PanelSpec` is what the factory
//! carries, which is why every field is `&'static` — a spec is never
//! built at runtime.

use geode_core::schema::ColumnType;
use geode_core::view::{Colour, ColumnFormat, Negative, Scale};

/// One document-level attribute the header paints (spec 2026-09-14 §4):
/// the column it reads, the short label the dense row shows, and the
/// declared type a typed edit is parsed as — on the SPEC for the same
/// reason `value_type` is (a `Snapshot` carries no declared type).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderAttr {
    pub column: &'static str,
    pub label: &'static str,
    pub ty: ColumnType,
}

/// How the columns across the top are chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Columns {
    /// Pivot: one column per distinct value of this axis, in the order
    /// the document lists them. The grid is then (row axis × this axis)
    /// and every cell is the document's one value column.
    Axis(&'static str),
    /// Flat: one row per document row, one column per value column the
    /// document declares. The shape a schedule takes — many rows, a
    /// handful of values each.
    Values,
}

/// One panel: the dataset it reads, how it lays a document out, what its
/// header shows, and how its numbers are formatted.
#[derive(Debug, Clone)]
pub struct PanelSpec {
    /// The roster kind, so the palette reads "CVI: Split".
    pub kind: &'static str,
    pub title: &'static str,
    pub dataset: &'static str,
    /// The document kind `:upload` writes back through (Part 4).
    pub document: &'static str,
    /// The axis down the side.
    pub rows: &'static str,
    pub columns: Columns,
    /// Document-level attributes shown in the header, in this order.
    pub header: &'static [HeaderAttr],
    pub format: ColumnFormat,
    /// The declared type of the value column(s) this panel's cells hold —
    /// what a typed cell edit is parsed as
    /// ([`crate::core::draft::parse_cell`]).
    ///
    /// It lives on the SPEC rather than being read off a delivered
    /// snapshot because a `Snapshot` carries no declared type at all
    /// (`ColumnMeta` is name + attribution + scope semantics): the type is
    /// the dataset's own declaration, and the panel — which already names
    /// its dataset, its axes and its format — is the one place in this
    /// crate that knows it. Reading it off the arrow array's runtime kind
    /// would be the wrong answer for the same reason a formatter is not a
    /// schema: an `i64` column whose values all happen to fit a `f64`
    /// array would then silently accept `0.5`.
    pub value_type: ColumnType,
}

impl PanelSpec {
    /// Whether this spec itself names `column` — the row axis, the pivot
    /// axis, or a header attribute.
    ///
    /// [`crate::core::matrix::MatrixModel::build`] reads the document key
    /// off the columns ahead of the row axis (`document_columns()` emits
    /// the key first, spec §3.3), and this is how it declines to count a
    /// column the panel is already painting somewhere else.
    pub fn names(&self, column: &str) -> bool {
        self.rows == column
            || matches!(self.columns, Columns::Axis(a) if a == column)
            || self.header.iter().any(|h| h.column == column)
    }
}

/// The CVI surface (spec §6.3/§8.1): one document per underlying, terms
/// down the side, nodes across the top, one `param` per cell.
///
/// `precision: 4` because a CVI parameter is a small number whose fourth
/// place is a real number a trader trades on; `thousands: false` for the
/// same reason (a grouped `1,234` would be a lie about the magnitude
/// anyone expects here), and no scale, since a parameter is already in
/// its own units. `Colour::None`: the sign of a CVI parameter carries no
/// good/bad meaning, so painting one red would invent a claim.
pub const CVI: PanelSpec = PanelSpec {
    kind: "cvi",
    title: "CVI",
    dataset: "cvi_params",
    document: "cvi_params",
    rows: "term",
    columns: Columns::Axis("node"),
    header: &[
        HeaderAttr {
            column: "anchor_date",
            label: "anchor",
            ty: ColumnType::Date,
        },
        HeaderAttr {
            column: "spot_ref",
            label: "spot",
            ty: ColumnType::F64,
        },
    ],
    value_type: ColumnType::F64,
    format: ColumnFormat {
        precision: 4,
        thousands: false,
        negative: Negative::Minus,
        colour: Colour::None,
        scale: Scale::None,
    },
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cvi_spec_names_its_own_axes_and_attributes_and_nothing_else() {
        assert!(CVI.names("term"));
        assert!(CVI.names("node"));
        assert!(CVI.names("anchor_date"));
        assert!(CVI.names("spot_ref"));
        assert!(
            !CVI.names("underlying_ref"),
            "the key is what `names` exists to leave uncounted"
        );
        assert!(!CVI.names("param"));
    }
}
