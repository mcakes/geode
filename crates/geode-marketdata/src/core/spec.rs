//! Market-data panel vocabulary. The types live in `geode_core::panel`,
//! where a panel reader can build them; this module re-exports them and
//! ships the builtin CVI and dividend panels as configuration.

use geode_core::config::{LayerDoc, merge_docs};
pub use geode_core::panel::{
    Columns, HeaderAttr, KindAction, KindActionRegistry, PanelSpec, RowAxis, RowIdentity, RowLabel,
    SliceValue, ValueColumn,
};
use geode_core::panel::{PANELS_DOC, read_panels};
use std::sync::{Arc, LazyLock};

/// Every kind action this crate's tile dispatches. Both CVI verbs are
/// unbuilt: the tile answers "not built yet" and the menu greys their rows.
pub const BUILTIN_KIND_ACTIONS: &[KindAction] = &[
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
];

/// [`BUILTIN_KIND_ACTIONS`] as a registry — what the composition root
/// registers and what this crate's builtin panels are read against.
pub fn builtin_kind_actions() -> KindActionRegistry {
    let mut registry = KindActionRegistry::default();
    for action in BUILTIN_KIND_ACTIONS {
        registry
            .register(*action)
            .expect("the builtin kind-action ids are distinct");
    }
    registry
}

/// CVI and DIVIDEND as builtin-layer configuration. `geode-app` layers it
/// under desk and user `panels.toml` and runs the full reader over the
/// merged document.
pub const BUILTIN_PANELS: &str = include_str!("builtin_panels.toml");

/// One builtin panel as its TOML reads, structurally. This crate cannot see
/// the schema or the document kinds, so the dataset checks are `geode-app`'s;
/// its tests load these same panels through the real composition. Panics on
/// a name the builtin document does not define — a programmer error. For
/// tests and benches; the application reads panels from its config.
pub fn builtin_panel(name: &str) -> Arc<PanelSpec> {
    static PANELS: LazyLock<Vec<Arc<PanelSpec>>> = LazyLock::new(|| {
        let doc = LayerDoc::builtin(PANELS_DOC, BUILTIN_PANELS)
            .expect("BUILTIN_PANELS is well-formed TOML");
        let (panels, diags) = read_panels(&merge_docs(PANELS_DOC, &[doc]), &builtin_kind_actions());
        assert!(diags.is_empty(), "the builtin panels read clean: {diags:?}");
        panels.into_iter().map(Arc::new).collect()
    });
    PANELS
        .iter()
        .find(|p| p.kind == name)
        .cloned()
        .unwrap_or_else(|| panic!("no builtin panel '{name}'"))
}

/// The builtin CVI panel: one document per underlying, terms by nodes.
pub static CVI: LazyLock<Arc<PanelSpec>> = LazyLock::new(|| builtin_panel("cvi"));

/// Closed dividend-status vocabulary. Feature crates do not depend on
/// sibling document implementations, so this declaration is checked against
/// `geode_documents::dividend::STATUSES` by a composition-root test in
/// `geode-app`; the demo generator's vocabulary is checked there too.
pub const STATUSES: [&str; 4] = ["estimated", "declared", "paid", "cancelled"];

/// The builtin dividend panel: one schedule per underlying, a row per payment.
pub static DIVIDEND: LazyLock<Arc<PanelSpec>> = LazyLock::new(|| builtin_panel("dividend"));

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::schema::ColumnType;
    use geode_core::view::ColumnFormat;

    /// Four places, no grouping, no scale, no sign colour: a parameter keeps its
    /// own units, and its sign carries no profit/loss meaning.
    const fn four_places() -> ColumnFormat {
        ColumnFormat {
            precision: 4,
            ..ColumnFormat::TEXT
        }
    }

    /// The CVI panel exactly as the compiled constant built it.
    fn expected_cvi() -> PanelSpec {
        PanelSpec {
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
            actions: BUILTIN_KIND_ACTIONS.to_vec(),
        }
    }

    /// The dividend panel exactly as the compiled constant built it.
    fn expected_dividend() -> PanelSpec {
        PanelSpec {
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
                    choices: Some(
                        ["estimated", "declared", "paid", "cancelled"]
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                    ),
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
        }
    }

    /// Every field of `loaded` against `expected`, one assertion each, so a
    /// drift names the field rather than dumping two whole panels.
    fn assert_same_panel(loaded: &PanelSpec, expected: &PanelSpec) {
        assert_eq!(loaded.kind, expected.kind, "kind");
        assert_eq!(loaded.title, expected.title, "title");
        assert_eq!(loaded.dataset, expected.dataset, "dataset");
        assert_eq!(loaded.document, expected.document, "document");
        assert_eq!(loaded.rows, expected.rows, "rows");
        assert_eq!(loaded.columns, expected.columns, "columns");
        assert_eq!(loaded.header, expected.header, "header");
        assert_eq!(loaded.slice_values, expected.slice_values, "slice values");
        assert_eq!(loaded.format, expected.format, "format");
        assert_eq!(loaded.value_type, expected.value_type, "value type");
        assert_eq!(loaded.actions, expected.actions, "actions");
        assert_eq!(loaded, expected, "and nothing else differs");
    }

    #[test]
    fn the_builtin_cvi_panel_is_the_spec_the_const_described() {
        assert_same_panel(&builtin_panel("cvi"), &expected_cvi());
    }

    #[test]
    fn the_builtin_dividend_panel_is_the_spec_the_const_described() {
        assert_same_panel(&builtin_panel("dividend"), &expected_dividend());
    }

    /// The builtin document reads clean, in shipped order — the order
    /// `geode-app` builds factories in, so `cvi` ships the keymap fragment.
    #[test]
    fn the_builtin_panels_read_clean_in_shipped_order() {
        let doc = merge_docs(
            PANELS_DOC,
            &[LayerDoc::builtin(PANELS_DOC, BUILTIN_PANELS).unwrap()],
        );
        let (panels, diags) = read_panels(&doc, &builtin_kind_actions());
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(
            panels.iter().map(|p| p.kind.as_str()).collect::<Vec<_>>(),
            ["cvi", "dividend"]
        );
    }

    #[test]
    fn the_cvi_spec_names_its_own_axes_and_attributes_and_nothing_else() {
        let cvi = builtin_panel("cvi");
        assert!(cvi.names("term"));
        assert!(cvi.names("node"));
        assert!(cvi.names("anchor_date"));
        assert!(cvi.names("spot_ref"));
        assert!(
            !cvi.names("underlying_ref"),
            "the key is what `names` exists to leave uncounted"
        );
        assert!(!cvi.names("param"));
    }

    /// The slice values are named too — that is how the pivot leaves
    /// them out of its "one value column" count — each with its own
    /// format: a forward is a price at two places, a vol at four.
    #[test]
    fn the_cvi_spec_names_its_slice_values_with_their_own_formats() {
        let cvi = builtin_panel("cvi");
        for column in ["forward", "atm", "skew"] {
            assert!(cvi.names(column), "{column}");
        }
        assert_eq!(cvi.slice_value("forward").unwrap().format.precision, 2);
        assert_eq!(cvi.slice_value("atm").unwrap().format.precision, 4);
        assert_eq!(cvi.slice_value("skew").unwrap().format.precision, 4);
        assert_eq!(
            cvi.slice_values
                .iter()
                .map(|s| s.label.as_str())
                .collect::<Vec<_>>(),
            ["fwd", "atm", "skew"]
        );
        assert!(cvi.slice_value("param").is_none());
    }

    /// Shipped specs hide only minted row identities. A hidden label still
    /// identifies the draft row; a typed axis needs a visible label editor.
    #[test]
    fn a_hidden_row_label_is_minted_on_every_shipped_spec() {
        let cvi = builtin_panel("cvi");
        let dividend = builtin_panel("dividend");
        assert_eq!(dividend.rows.label, RowLabel::Hidden);
        assert_eq!(cvi.rows.label, RowLabel::Shown);
        for spec in [&cvi, &dividend] {
            if spec.rows.label == RowLabel::Hidden {
                assert_eq!(spec.rows.identity, RowIdentity::Minted, "{}", spec.kind);
            }
        }
        assert!(!dividend.rows.shown());
        assert!(cvi.rows.shown());
    }

    #[test]
    fn a_flat_spec_names_its_value_columns() {
        let cvi = builtin_panel("cvi");
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
        assert!(cvi.flat_columns().is_empty());
        assert_eq!(cvi.rows.identity, RowIdentity::Typed(ColumnType::Date));
    }

    /// The dividend panel declares all flat value columns and both header
    /// attributes, with row identities minted by the panel.
    #[test]
    fn the_dividend_spec_names_its_own_columns_and_header_attributes() {
        let dividend = builtin_panel("dividend");
        assert_eq!(dividend.kind, "dividend");
        assert_eq!(dividend.rows.identity, RowIdentity::Minted);
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
            assert!(dividend.names(column), "{column}");
        }
        assert!(!dividend.names("underlying_ref"));
        assert_eq!(dividend.flat_columns().len(), 5);
        assert!(dividend.slice_values.is_empty());
    }

    /// Every dividend column is required. Status alone carries the closed
    /// vocabulary whose consistency with document parsing is tested by
    /// `geode-app`, where both declarations are available.
    #[test]
    fn the_dividend_spec_requires_every_column() {
        let dividend = builtin_panel("dividend");
        let required = |label: &str| dividend.value_column(label).unwrap().required;
        assert!(required("ex_date"));
        assert!(required("announced_date"));
        assert!(required("pay_date"));
        assert!(required("amount"));
        assert!(required("status"));
        assert_eq!(
            dividend
                .value_column("status")
                .unwrap()
                .choices
                .as_deref()
                .map(|c| c.iter().map(String::as_str).collect::<Vec<_>>()),
            Some(vec!["estimated", "declared", "paid", "cancelled"])
        );
    }

    #[test]
    fn dividend_dates_are_required() {
        let dividend = builtin_panel("dividend");
        let cols = dividend.flat_columns();
        for c in ["announced_date", "pay_date", "ex_date", "amount", "status"] {
            assert!(cols.iter().find(|v| v.column == c).unwrap().required, "{c}");
        }
    }

    /// The builtin verbs are CVI's two, both unbuilt, and CVI offers exactly
    /// the registry's entries — the menu and dispatch read them through it.
    #[test]
    fn the_builtin_kind_actions_are_cvis_two_unbuilt_verbs() {
        let registry = builtin_kind_actions();
        assert_eq!(
            registry.ids(),
            vec!["marketdata::cvi_reanchor", "marketdata::cvi_recalc_forward"]
        );
        assert!(BUILTIN_KIND_ACTIONS.iter().all(|a| !a.built));
        assert_eq!(builtin_panel("cvi").actions, BUILTIN_KIND_ACTIONS.to_vec());
    }
}
