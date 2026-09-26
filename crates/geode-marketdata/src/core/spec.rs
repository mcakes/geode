//! Compiled panel definitions for CVI pivots and dividend schedules.
//! Factories hold `&'static PanelSpec` values; column names, labels, and
//! choice vocabularies are static program data rather than user config.

use geode_core::schema::ColumnType;
use geode_core::view::{Colour, ColumnFormat, Negative, Scale};

/// A header attribute's source column, display label, and edit type.
/// The declared type belongs here because snapshot metadata does not
/// carry the dataset's schema type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderAttr {
    pub column: &'static str,
    pub label: &'static str,
    pub ty: ColumnType,
}

/// A kind-specific action advertised in the menu, palette, and keymap.
/// `built: false` disables the menu row with a reason. The tile's action
/// dispatcher owns execution; this declaration performs no computation or I/O.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindAction {
    pub id: &'static str,
    pub title: &'static str,
    pub built: bool,
}

/// A value constant across a row-axis slice, such as CVI's forward per
/// term. The long document repeats it on every node row; the grid paints
/// it before the ladder using its own format. Row bumps skip these columns.
///
/// Slice values are read and uploaded as `f64`; their editors use the
/// panel's `value_type`, which must therefore be compatible.
#[derive(Debug, Clone, PartialEq)]
pub struct SliceValue {
    pub column: &'static str,
    /// The short column header. **Must not collide with any label the
    /// column axis can produce** (or another slice value's): `Draft`
    /// resolves an edit by `(row label, column label)` and indexes
    /// `MatrixModel.columns` by label, so the pivot refuses a document
    /// whose axis produces this label rather than paint two columns one
    /// name.
    pub label: &'static str,
    pub format: ColumnFormat,
}

/// Who names a new row — the trader (`Typed`, the row-label editor opens
/// on insert, parsed as the given type) or the panel itself (`Minted`,
/// `new-<n>`, no editor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowIdentity {
    Typed(ColumnType),
    Minted,
}

/// Whether to paint a row-label column. Hidden labels still identify draft
/// rows across restoration and rebase; table column 0 becomes the first value,
/// and search/copy operate on painted cells. Shipped specs hide only minted
/// labels, since typed row identities need a visible row-label editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowLabel {
    Shown,
    Hidden,
}

/// The axis down the side: the column a row's label comes from, who
/// gets to choose it, and whether it is painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowAxis {
    pub column: &'static str,
    pub identity: RowIdentity,
    pub label: RowLabel,
}

impl RowAxis {
    /// Whether the table carries a row-label column.
    pub fn shown(&self) -> bool {
        self.label == RowLabel::Shown
    }
}

/// One flat column: what it reads, how it paints, how it is edited,
/// whether an inserted row must fill it.
#[derive(Debug, Clone, PartialEq)]
pub struct ValueColumn {
    pub column: &'static str,
    pub label: &'static str,
    pub ty: ColumnType,
    pub format: ColumnFormat,
    pub choices: Option<&'static [&'static str]>,
    pub required: bool,
}

/// How the columns across the top are chosen.
#[derive(Debug, Clone, PartialEq)]
pub enum Columns {
    /// Pivot: one column per distinct value of this axis, in the order
    /// the document lists them. The grid is then (row axis × this axis)
    /// and every cell is the document's one value column.
    Axis(&'static str),
    /// Flat: one row per document row, one column per listed value
    /// column. The shape a schedule takes — many rows, a handful of
    /// values each.
    Values(&'static [ValueColumn]),
}

/// One panel: the dataset it reads, how it lays a document out, what its
/// header shows, and how its numbers are formatted.
#[derive(Debug, Clone)]
pub struct PanelSpec {
    /// The roster kind, so the palette reads "CVI: Split".
    pub kind: &'static str,
    pub title: &'static str,
    pub dataset: &'static str,
    /// The document kind used to serialize `:upload`.
    pub document: &'static str,
    /// The axis down the side.
    pub rows: RowAxis,
    pub columns: Columns,
    /// Document-level attributes shown in the header, in this order.
    pub header: &'static [HeaderAttr],
    /// Per-slice values painted ahead of the ladder, in this order
    /// (`Columns::Axis` only; a flat panel has no slice).
    pub slice_values: &'static [SliceValue],
    pub format: ColumnFormat,
    /// Declared numeric edit and upload type for a pivot's ladder. Flat
    /// columns declare their own types in [`ValueColumn`]. Snapshot metadata
    /// does not expose schema types, and the runtime array representation
    /// alone cannot decide whether an editor may accept fractional input.
    pub value_type: ColumnType,
    /// Kind-specific actions listed under this panel's title and registered
    /// for palette and keymap dispatch.
    pub actions: &'static [KindAction],
}

impl PanelSpec {
    /// Whether the panel names this row axis, pivot axis, flat value, header
    /// attribute, or slice value. Key extraction excludes these columns from
    /// the prefix before the row axis.
    pub fn names(&self, column: &str) -> bool {
        self.rows.column == column
            || matches!(self.columns, Columns::Axis(a) if a == column)
            || self.value_column(column).is_some()
            || self.header.iter().any(|h| h.column == column)
            || self.slice_value(column).is_some()
    }

    /// The slice value this spec paints from `column`, if any — how the
    /// pivot tells a spec-named per-slice value from a second value
    /// column it must refuse (both arrive `DeterminedNonAdditive`).
    pub fn slice_value(&self, column: &str) -> Option<&SliceValue> {
        self.slice_values.iter().find(|s| s.column == column)
    }

    /// The flat column this spec paints from `column`, if the layout is
    /// flat and lists it.
    pub fn value_column(&self, column: &str) -> Option<&ValueColumn> {
        self.flat_columns().iter().find(|c| c.column == column)
    }

    /// The flat layout's columns in paint order; empty under a pivot.
    pub fn flat_columns(&self) -> &'static [ValueColumn] {
        match self.columns {
            Columns::Values(cols) => cols,
            Columns::Axis(_) => &[],
        }
    }
}

/// CVI's cell format: four places, no grouping, no scale, no sign colour
/// (see [`CVI`]). The slice values copy it and change only `precision`.
const CVI_FORMAT: ColumnFormat = ColumnFormat {
    precision: 4,
    thousands: false,
    negative: Negative::Minus,
    colour: Colour::None,
    scale: Scale::None,
};

/// One CVI document per underlying, with terms as rows and nodes as columns.
/// Parameters retain their own units at four decimals, without grouping or
/// scaling. Sign carries no profit/loss meaning, so it has no sign colour.
pub const CVI: PanelSpec = PanelSpec {
    kind: "cvi",
    title: "CVI",
    dataset: "cvi_params",
    document: "cvi_params",
    rows: RowAxis {
        column: "term",
        identity: RowIdentity::Typed(ColumnType::Date),
        label: RowLabel::Shown,
    },
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
    slice_values: &[
        SliceValue {
            column: "forward",
            label: "fwd",
            format: ColumnFormat {
                precision: 2,
                ..CVI_FORMAT
            },
        },
        SliceValue {
            column: "atm",
            label: "atm",
            format: CVI_FORMAT,
        },
        SliceValue {
            column: "skew",
            label: "skew",
            format: CVI_FORMAT,
        },
    ],
    value_type: ColumnType::F64,
    format: CVI_FORMAT,
    actions: &[
        KindAction {
            id: "marketdata::cvi_reanchor",
            title: "Reanchor",
            built: false,
        },
        KindAction {
            id: "marketdata::cvi_recalc_forward",
            title: "Recalc forward",
            built: false,
        },
    ],
};

/// Closed dividend-status vocabulary. Feature crates do not depend on
/// sibling document implementations, so this declaration is checked against
/// `geode_documents::dividend::STATUSES` by a composition-root test in
/// `geode-app`; the demo generator's vocabulary is checked there too.
pub const STATUSES: [&str; 4] = ["estimated", "declared", "paid", "cancelled"];

/// Dividend amounts display four decimals to retain fractional cents,
/// without grouping or scaling. Sign colour is disabled because this is
/// a payment amount rather than a profit/loss measure.
const DIVIDEND_FORMAT: ColumnFormat = ColumnFormat {
    precision: 4,
    thousands: false,
    negative: Negative::Minus,
    colour: Colour::None,
    scale: Scale::None,
};

/// One dividend schedule per underlying, with a row per payment and typed
/// flat value columns. Minted row IDs provide identity without a visible
/// label column or a user-entered row name.
///
/// Dates paint as ISO text and status as literal text, so their formats
/// use [`ColumnFormat::TEXT`]. Every column is required for inserted rows,
/// including announced and pay dates: an estimated schedule still supplies
/// estimates for those dates. Status alone has a closed choice vocabulary.
pub const DIVIDEND: PanelSpec = PanelSpec {
    kind: "dividend",
    title: "Dividend",
    dataset: "dividend_schedule",
    document: "dividend_schedule",
    rows: RowAxis {
        column: "dividend_id",
        identity: RowIdentity::Minted,
        label: RowLabel::Hidden,
    },
    columns: Columns::Values(&[
        ValueColumn {
            column: "ex_date",
            label: "ex",
            ty: ColumnType::Date,
            format: ColumnFormat::TEXT,
            choices: None,
            required: true,
        },
        ValueColumn {
            column: "announced_date",
            label: "announced",
            ty: ColumnType::Date,
            format: ColumnFormat::TEXT,
            choices: None,
            required: true,
        },
        ValueColumn {
            column: "pay_date",
            label: "pay",
            ty: ColumnType::Date,
            format: ColumnFormat::TEXT,
            choices: None,
            required: true,
        },
        ValueColumn {
            column: "amount",
            label: "amount",
            ty: ColumnType::F64,
            format: DIVIDEND_FORMAT,
            choices: None,
            required: true,
        },
        ValueColumn {
            column: "status",
            label: "status",
            ty: ColumnType::Utf8,
            format: ColumnFormat::TEXT,
            choices: Some(&STATUSES),
            required: true,
        },
    ]),
    header: &[
        HeaderAttr {
            column: "currency",
            label: "ccy",
            ty: ColumnType::Utf8,
        },
        HeaderAttr {
            column: "schedule_date",
            label: "struck",
            ty: ColumnType::Date,
        },
    ],
    slice_values: &[],
    value_type: ColumnType::F64,
    format: DIVIDEND_FORMAT,
    actions: &[],
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

    /// The slice values are named too — that is how the pivot leaves
    /// them out of its "one value column" count — each with its own
    /// format: a forward is a price at two places, a vol at four.
    #[test]
    fn the_cvi_spec_names_its_slice_values_with_their_own_formats() {
        for column in ["forward", "atm", "skew"] {
            assert!(CVI.names(column), "{column}");
        }
        assert_eq!(CVI.slice_value("forward").unwrap().format.precision, 2);
        assert_eq!(CVI.slice_value("atm").unwrap().format.precision, 4);
        assert_eq!(CVI.slice_value("skew").unwrap().format.precision, 4);
        assert_eq!(
            CVI.slice_values.iter().map(|s| s.label).collect::<Vec<_>>(),
            ["fwd", "atm", "skew"]
        );
        assert!(CVI.slice_value("param").is_none());
    }

    /// Shipped specs hide only minted row identities. A hidden label still
    /// identifies the draft row; a typed axis needs a visible label editor.
    #[test]
    fn a_hidden_row_label_is_minted_on_every_shipped_spec() {
        assert_eq!(DIVIDEND.rows.label, RowLabel::Hidden);
        assert_eq!(CVI.rows.label, RowLabel::Shown);
        for spec in [&CVI, &DIVIDEND] {
            if spec.rows.label == RowLabel::Hidden {
                assert_eq!(spec.rows.identity, RowIdentity::Minted, "{}", spec.kind);
            }
        }
        assert!(!DIVIDEND.rows.shown());
        assert!(CVI.rows.shown());
    }

    #[test]
    fn a_flat_spec_names_its_value_columns() {
        const FLAT: PanelSpec = PanelSpec {
            kind: "flat",
            title: "Flat",
            dataset: "d",
            document: "d",
            rows: RowAxis {
                column: "id",
                identity: RowIdentity::Minted,
                label: RowLabel::Shown,
            },
            columns: Columns::Values(&[
                ValueColumn {
                    column: "amount",
                    label: "amount",
                    ty: ColumnType::F64,
                    format: CVI_FORMAT,
                    choices: None,
                    required: true,
                },
                ValueColumn {
                    column: "status",
                    label: "status",
                    ty: ColumnType::Utf8,
                    format: CVI_FORMAT,
                    choices: Some(&["a", "b"]),
                    required: false,
                },
            ]),
            header: &[],
            slice_values: &[],
            value_type: ColumnType::F64,
            format: CVI_FORMAT,
            actions: &[],
        };
        assert!(FLAT.names("id"));
        assert!(FLAT.names("amount"));
        assert!(FLAT.names("status"));
        assert!(!FLAT.names("underlying_ref"));
        assert_eq!(
            FLAT.value_column("status").unwrap().choices,
            Some(&["a", "b"][..])
        );
        assert!(FLAT.value_column("nope").is_none());
        assert_eq!(FLAT.flat_columns().len(), 2);
        assert!(CVI.flat_columns().is_empty());
        assert_eq!(CVI.rows.identity, RowIdentity::Typed(ColumnType::Date));
    }

    /// The dividend panel declares all flat value columns and both header
    /// attributes, with row identities minted by the panel.
    #[test]
    fn the_dividend_spec_names_its_own_columns_and_header_attributes() {
        assert_eq!(DIVIDEND.kind, "dividend");
        assert_eq!(DIVIDEND.rows.identity, RowIdentity::Minted);
        for column in [
            "dividend_id",
            "ex_date",
            "announced_date",
            "pay_date",
            "amount",
            "status",
            "currency",
            "schedule_date",
        ] {
            assert!(DIVIDEND.names(column), "{column}");
        }
        assert!(!DIVIDEND.names("underlying_ref"));
        assert_eq!(DIVIDEND.flat_columns().len(), 5);
        assert!(DIVIDEND.slice_values.is_empty());
    }

    /// Every dividend column is required. Status alone carries the closed
    /// vocabulary whose consistency with document parsing is tested by
    /// `geode-app`, where both declarations are available.
    #[test]
    fn the_dividend_spec_requires_every_column() {
        let required = |label: &str| DIVIDEND.value_column(label).unwrap().required;
        assert!(required("ex_date"));
        assert!(required("announced_date"));
        assert!(required("pay_date"));
        assert!(required("amount"));
        assert!(required("status"));
        assert_eq!(
            DIVIDEND.value_column("status").unwrap().choices,
            Some(&STATUSES[..])
        );
        assert_eq!(STATUSES, ["estimated", "declared", "paid", "cancelled"]);
    }

    #[test]
    fn dividend_dates_are_required() {
        let Columns::Values(cols) = DIVIDEND.columns else {
            panic!()
        };
        for c in ["announced_date", "pay_date", "ex_date", "amount", "status"] {
            assert!(cols.iter().find(|v| v.column == c).unwrap().required, "{c}");
        }
    }
}
