//! Market-data panel vocabulary. The types live in `geode_core::panel`,
//! where a panel reader can build them; this module re-exports them and
//! holds the compiled-in CVI and dividend panels.

pub use geode_core::panel::{
    Columns, HeaderAttr, KindAction, PanelSpec, RowAxis, RowIdentity, RowLabel, SliceValue,
    ValueColumn,
};
use geode_core::schema::ColumnType;
use geode_core::view::ColumnFormat;
use std::sync::{Arc, LazyLock};

/// Four places, no grouping, no scale, no sign colour: a parameter keeps its
/// own units, and its sign carries no profit/loss meaning.
const fn four_places() -> ColumnFormat {
    ColumnFormat {
        precision: 4,
        ..ColumnFormat::TEXT
    }
}

/// One CVI document per underlying, with terms as rows and nodes as columns.
/// Parameters retain their own units at four decimals, without grouping or
/// scaling. Sign carries no profit/loss meaning, so it has no sign colour.
pub static CVI: LazyLock<Arc<PanelSpec>> = LazyLock::new(|| {
    Arc::new(PanelSpec {
        kind: "cvi".into(),
        title: "CVI".into(),
        dataset: "cvi_params".into(),
        document: "cvi_params".into(),
        rows: RowAxis {
            column: "term".into(),
            identity: RowIdentity::Typed(ColumnType::Date),
            label: RowLabel::Shown,
        },
        columns: Columns::Axis("node".into()),
        header: vec![
            HeaderAttr {
                column: "anchor_date".into(),
                label: "anchor".into(),
                ty: ColumnType::Date,
            },
            HeaderAttr {
                column: "spot_ref".into(),
                label: "spot".into(),
                ty: ColumnType::F64,
            },
        ],
        slice_values: vec![
            SliceValue {
                column: "forward".into(),
                label: "fwd".into(),
                format: ColumnFormat {
                    precision: 2,
                    ..four_places()
                },
            },
            SliceValue {
                column: "atm".into(),
                label: "atm".into(),
                format: four_places(),
            },
            SliceValue {
                column: "skew".into(),
                label: "skew".into(),
                format: four_places(),
            },
        ],
        value_type: ColumnType::F64,
        format: four_places(),
        actions: vec![
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
    })
});

/// Closed dividend-status vocabulary. Feature crates do not depend on
/// sibling document implementations, so this declaration is checked against
/// `geode_documents::dividend::STATUSES` by a composition-root test in
/// `geode-app`; the demo generator's vocabulary is checked there too.
pub const STATUSES: [&str; 4] = ["estimated", "declared", "paid", "cancelled"];

/// One dividend schedule per underlying, with a row per payment and typed
/// flat value columns. Minted row IDs provide identity without a visible
/// label column or a user-entered row name.
///
/// Dates paint as ISO text and status as literal text, so their formats
/// use [`ColumnFormat::TEXT`]. Amounts display four decimals to retain
/// fractional cents. Every column is required for inserted rows, including
/// announced and pay dates: an estimated schedule still supplies estimates
/// for those dates. Status alone has a closed choice vocabulary.
pub static DIVIDEND: LazyLock<Arc<PanelSpec>> = LazyLock::new(|| {
    Arc::new(PanelSpec {
        kind: "dividend".into(),
        title: "Dividend".into(),
        dataset: "dividend_schedule".into(),
        document: "dividend_schedule".into(),
        rows: RowAxis {
            column: "dividend_id".into(),
            identity: RowIdentity::Minted,
            label: RowLabel::Hidden,
        },
        columns: Columns::Values(vec![
            ValueColumn {
                column: "ex_date".into(),
                label: "ex".into(),
                ty: ColumnType::Date,
                format: ColumnFormat::TEXT,
                choices: None,
                required: true,
            },
            ValueColumn {
                column: "announced_date".into(),
                label: "announced".into(),
                ty: ColumnType::Date,
                format: ColumnFormat::TEXT,
                choices: None,
                required: true,
            },
            ValueColumn {
                column: "pay_date".into(),
                label: "pay".into(),
                ty: ColumnType::Date,
                format: ColumnFormat::TEXT,
                choices: None,
                required: true,
            },
            ValueColumn {
                column: "amount".into(),
                label: "amount".into(),
                ty: ColumnType::F64,
                format: four_places(),
                choices: None,
                required: true,
            },
            ValueColumn {
                column: "status".into(),
                label: "status".into(),
                ty: ColumnType::Utf8,
                format: ColumnFormat::TEXT,
                choices: Some(STATUSES.iter().map(|s| s.to_string()).collect()),
                required: true,
            },
        ]),
        header: vec![
            HeaderAttr {
                column: "currency".into(),
                label: "ccy".into(),
                ty: ColumnType::Utf8,
            },
            HeaderAttr {
                column: "schedule_date".into(),
                label: "struck".into(),
                ty: ColumnType::Date,
            },
        ],
        slice_values: Vec::new(),
        value_type: ColumnType::F64,
        format: four_places(),
        actions: Vec::new(),
    })
});

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
            CVI.slice_values
                .iter()
                .map(|s| s.label.as_str())
                .collect::<Vec<_>>(),
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
        for spec in [&*CVI, &*DIVIDEND] {
            if spec.rows.label == RowLabel::Hidden {
                assert_eq!(spec.rows.identity, RowIdentity::Minted, "{}", spec.kind);
            }
        }
        assert!(!DIVIDEND.rows.shown());
        assert!(CVI.rows.shown());
    }

    #[test]
    fn a_flat_spec_names_its_value_columns() {
        let flat = PanelSpec {
            kind: "flat".into(),
            title: "Flat".into(),
            dataset: "d".into(),
            document: "d".into(),
            rows: RowAxis {
                column: "id".into(),
                identity: RowIdentity::Minted,
                label: RowLabel::Shown,
            },
            columns: Columns::Values(vec![
                ValueColumn {
                    column: "amount".into(),
                    label: "amount".into(),
                    ty: ColumnType::F64,
                    format: four_places(),
                    choices: None,
                    required: true,
                },
                ValueColumn {
                    column: "status".into(),
                    label: "status".into(),
                    ty: ColumnType::Utf8,
                    format: four_places(),
                    choices: Some(vec!["a".to_string(), "b".to_string()].into()),
                    required: false,
                },
            ]),
            header: Vec::new(),
            slice_values: Vec::new(),
            value_type: ColumnType::F64,
            format: four_places(),
            actions: Vec::new(),
        };
        assert!(flat.names("id"));
        assert!(flat.names("amount"));
        assert!(flat.names("status"));
        assert!(!flat.names("underlying_ref"));
        assert_eq!(
            flat.value_column("status").unwrap().choices.as_deref(),
            Some(&["a".to_string(), "b".to_string()][..])
        );
        assert!(flat.value_column("nope").is_none());
        assert_eq!(flat.flat_columns().len(), 2);
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
            DIVIDEND
                .value_column("status")
                .unwrap()
                .choices
                .as_deref()
                .map(|c| c.iter().map(String::as_str).collect::<Vec<_>>()),
            Some(STATUSES.to_vec())
        );
        assert_eq!(STATUSES, ["estimated", "declared", "paid", "cancelled"]);
    }

    #[test]
    fn dividend_dates_are_required() {
        let Columns::Values(cols) = &DIVIDEND.columns else {
            panic!()
        };
        for c in ["announced_date", "pay_date", "ex_date", "amount", "status"] {
            assert!(cols.iter().find(|v| v.column == c).unwrap().required, "{c}");
        }
    }
}
