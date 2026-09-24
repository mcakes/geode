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

/// A verb this document kind owns (spec 2026-09-14 §6.3): listed in the
/// panel's action menu under the kind's own section, registered as an
/// action so the palette and a keymap reach it. `built: false` paints
/// greyed "not built yet"; when built it is an egress REQUEST to the
/// upstream system (charter: Geode computes nothing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindAction {
    pub id: &'static str,
    pub title: &'static str,
    pub built: bool,
}

/// One value a document says once per ROW-AXIS slice rather than once
/// per cell (2026-09-17): CVI's `forward`/`atm`/`skew` per term. Stored
/// in the long form repeated on every node row of its slice, painted as
/// the first grid columns ahead of the pivot's own ladder, each with its
/// own format — a forward is a price, an ATM vol a decimal — and skipped
/// by a row bump, which walks the ladder alone.
///
/// A slice value is `f64` only: its cell is edited through the spec's
/// `value_type` exactly as a ladder cell is, and the pivot's
/// within-slice disagreement check reads it through `Snapshot::f64_at`.
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

/// Whether the row label gets a column of its own. `Hidden` withholds
/// it (user ruling 2026-09-20: a feed's opaque `dividend_id` means
/// nothing to a trader) — the label is still the row's IDENTITY for the
/// draft, the session and every rebase; it is just not painted, so the
/// table's column 0 is the first value column, `/` searches the painted
/// cells and `yy` copies them alone. A hidden label can only be `Minted`:
/// a `Typed` axis needs the label column to type into.
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
    /// The document kind `:upload` writes back through (Part 4).
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
    /// Verbs the kind itself owns (spec §6.3): listed in the panel's
    /// action menu under this spec's own `title` section, registered
    /// through `register_actions` so the palette and a keymap reach them.
    pub actions: &'static [KindAction],
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

/// The dividend schedule's closed status vocabulary (design spec §6.1,
/// §6.5). Copied here rather than imported: `geode-marketdata` must not
/// depend on `geode-documents` (CLAUDE.md's layering rules — a module
/// crate is one of "the modules" the shell/data boundary is drawn
/// around, and `geode-documents` sits below it), so this and
/// `geode_documents::dividend::STATUSES` are two declarations of the
/// same four words. A `geode-app` test — the one crate where every layer
/// meets — asserts they agree; `geode-demo-data`'s own copy (fed to the
/// generator) is checked against `geode-documents`' the same way.
pub const STATUSES: [&str; 4] = ["estimated", "declared", "paid", "cancelled"];

/// The dividend schedule's cell format (spec §6.5): four places, because
/// a per-share amount can carry fractional cents a trader trades on
/// exactly as a CVI parameter's fourth place does (see [`CVI_FORMAT`]);
/// no grouping, since a dividend amount never reaches a size where
/// `1,234` reads as anything but noise; and — unlike a P&L figure —
/// `Colour::None` rather than `Colour::Sign`, because every dividend
/// amount here is a positive per-share payment: there is no "bad" sign
/// for red to mark, and colouring it anyway would paint a claim the
/// number itself never makes.
const DIVIDEND_FORMAT: ColumnFormat = ColumnFormat {
    precision: 4,
    thousands: false,
    negative: Negative::Minus,
    colour: Colour::None,
    scale: Scale::None,
};

/// The dividend schedule surface (spec §6.4/§6.5): one document per
/// underlying, one row per scheduled dividend. `Columns::Values`, not a
/// pivot — a schedule is a handful of rows with a handful of values
/// each, the shape that variant's own doc comment describes — and rows
/// are minted by the panel (`RowIdentity::Minted`) rather than typed,
/// since a dividend's identity is the document's own `dividend_id`, a
/// generated key rather than a value a trader would ever type.
///
/// The three date columns (`ex`/`announced`/`pay`) share
/// [`ColumnFormat::TEXT`]: precision, grouping, sign and scale are all
/// irrelevant to them, because [`crate::core::matrix::cell_text`] paints
/// every `Value::Date` as `%Y-%m-%d` regardless of what the column's
/// format says — the same reason `status`, also `Utf8`, uses it too.
///
/// Every column is `required`
/// ([`Draft::incomplete_rows`](crate::core::draft::Draft::incomplete_rows)'s
/// gate on when an inserted row counts as complete, spec §5.2), including
/// `announced_date` and `pay_date` (ruling 2026-09-23): an undeclared
/// dividend still carries an ISSUER-ESTIMATED announce and pay date on the
/// wire, so a trader inserting a row ahead of the formal declaration types
/// the estimate the desk is already working from rather than leaving the
/// row incomplete for dates that in fact already exist, just not yet as
/// facts the issuer has confirmed. `status` is the one column with a
/// closed vocabulary (`choices`), stepped in place exactly as a config
/// dialog's `Choice` field is (spec §4.4).
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

    /// §4.2: a flat panel names its columns; `names` covers them, and
    /// `value_column` answers each by name so the build can refuse a
    /// value the spec does not list rather than paint it unlabelled.
    /// A hidden row label (user ruling 2026-09-20, "dividend_id shouldn't
    /// be displayed") is still the row's identity — only its column is
    /// withheld — and it can only be minted: a `Typed` axis needs the
    /// label column to type into.
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

    /// The second panel spec (dividend design spec §6.5): a flat layout
    /// (`Columns::Values`, so `flat_columns` is non-empty and `names`
    /// covers every listed column plus the two header attributes) with
    /// rows the panel itself mints rather than the trader typing a
    /// label.
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

    /// Every column is required (see [`DIVIDEND`]'s own doc comment for
    /// why `announced`/`pay` are, ruling 2026-09-23), and `status` alone
    /// carries a closed vocabulary — the one [`STATUSES`] this crate must
    /// keep in step with `geode_documents::dividend::STATUSES` (checked
    /// in `geode-app`, the one crate where both are visible).
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
