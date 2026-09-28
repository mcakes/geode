use super::*;
use crate::schema::ColumnType;
use crate::view::ColumnFormat;

/// A flat panel with a minted row axis, two value columns and one header
/// attribute — every part `names` must count.
fn flat() -> PanelSpec {
    PanelSpec {
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
                format: ColumnFormat::TEXT,
                choices: None,
                required: true,
            },
            ValueColumn {
                column: "status".into(),
                label: "status".into(),
                ty: ColumnType::Utf8,
                format: ColumnFormat::TEXT,
                choices: Some(vec!["a".to_string(), "b".to_string()].into()),
                required: false,
            },
        ]),
        header: vec![HeaderAttr {
            column: "currency".into(),
            label: "ccy".into(),
            ty: ColumnType::Utf8,
        }],
        slice_values: Vec::new(),
        format: ColumnFormat::TEXT,
        value_type: ColumnType::F64,
        actions: Vec::new(),
    }
}

/// `flat()` laid out as a pivot over `node` with one slice value.
fn pivot() -> PanelSpec {
    PanelSpec {
        columns: Columns::Axis("node".into()),
        slice_values: vec![SliceValue {
            column: "forward".into(),
            label: "fwd".into(),
            format: ColumnFormat::TEXT,
        }],
        ..flat()
    }
}

#[test]
fn a_flat_panel_names_its_row_axis_values_and_header_and_nothing_else() {
    let spec = flat();
    for column in ["id", "amount", "status", "currency"] {
        assert!(spec.names(column), "{column}");
    }
    assert!(
        !spec.names("underlying_ref"),
        "the key is what `names` exists to leave uncounted"
    );
    assert_eq!(spec.flat_columns().len(), 2);
    assert_eq!(
        spec.value_column("status").unwrap().choices.as_deref(),
        Some(&["a".to_string(), "b".to_string()][..])
    );
    assert!(spec.value_column("nope").is_none());
    assert!(spec.slice_value("amount").is_none());
}

#[test]
fn a_pivot_names_its_axis_and_slices_and_has_no_flat_columns() {
    let spec = pivot();
    assert!(spec.names("node"));
    assert!(spec.names("forward"));
    assert_eq!(spec.slice_value("forward").unwrap().label, "fwd");
    assert!(spec.flat_columns().is_empty());
    assert!(spec.value_column("amount").is_none());
}

#[test]
fn only_a_shown_row_label_paints_a_label_column() {
    let mut spec = flat();
    assert!(spec.rows.shown());
    spec.rows.label = RowLabel::Hidden;
    assert!(!spec.rows.shown());
}

const REANCHOR: KindAction = KindAction {
    id: "marketdata::cvi_reanchor",
    title: "Reanchor",
    built: false,
};

#[test]
fn a_registered_kind_action_is_found_by_id_and_a_second_registration_is_refused() {
    let mut registry = KindActionRegistry::default();
    registry.register(REANCHOR).unwrap();
    assert_eq!(registry.get("marketdata::cvi_reanchor"), Some(REANCHOR));
    assert_eq!(registry.get("marketdata::nonesuch"), None);
    assert_eq!(registry.ids(), vec!["marketdata::cvi_reanchor"]);
    let again = KindAction {
        title: "Other",
        ..REANCHOR
    };
    assert!(
        registry.register(again).is_err(),
        "one id, one verb: a second registration would change what a panel's menu row does"
    );
    assert_eq!(
        registry.get("marketdata::cvi_reanchor").unwrap().title,
        "Reanchor"
    );
}
