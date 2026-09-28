use super::*;
use crate::config::{Diagnostic, Layer, LayerDoc, MergedDoc, Severity, merge_docs};
use crate::schema::ColumnType;
use crate::view::ColumnFormat;
use crate::view::{Colour, Negative, Scale};

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

/// The shipped CVI panel's shape: a pivot with a header and three slices.
const CVI_PANEL: &str = r#"
[cvi]
title = "CVI"
dataset = "cvi_params"
document = "cvi_params"
actions = ["marketdata::cvi_reanchor", "marketdata::cvi_recalc_forward"]

[cvi.value]
type = "f64"
format = { precision = 4 }

[cvi.rows]
column = "term"
identity = "date"
label = "shown"

[cvi.columns]
axis = "node"

[[cvi.header]]
column = "anchor_date"
label = "anchor"
type = "date"

[[cvi.header]]
column = "spot_ref"
label = "spot"
type = "f64"

[[cvi.slice]]
column = "forward"
label = "fwd"
format = { precision = 2 }

[[cvi.slice]]
column = "atm"
label = "atm"

[[cvi.slice]]
column = "skew"
label = "skew"
"#;

/// The shipped dividend panel's shape: flat, minted hidden rows, choices.
const DIVIDEND_PANEL: &str = r#"
[dividend]
title = "Dividend"
dataset = "dividend_schedule"
document = "dividend_schedule"

[dividend.value]
type = "f64"
format = { precision = 4 }

[dividend.rows]
column = "dividend_id"
identity = "minted"
label = "hidden"

[[dividend.columns.values]]
column = "ex_date"
label = "ex"
type = "date"
required = true

[[dividend.columns.values]]
column = "announced_date"
label = "announced"
type = "date"
required = true

[[dividend.columns.values]]
column = "pay_date"
label = "pay"
type = "date"
required = true

[[dividend.columns.values]]
column = "amount"
label = "amount"
type = "f64"
format = { precision = 4 }
required = true

[[dividend.columns.values]]
column = "status"
label = "status"
type = "utf8"
choices = ["estimated", "declared", "paid", "cancelled"]
required = true

[[dividend.header]]
column = "currency"
label = "ccy"
type = "utf8"

[[dividend.header]]
column = "schedule_date"
label = "struck"
type = "date"
"#;

fn actions() -> KindActionRegistry {
    let mut registry = KindActionRegistry::default();
    for (id, title) in [
        ("marketdata::cvi_reanchor", "Reanchor"),
        ("marketdata::cvi_recalc_forward", "Recalc forward"),
    ] {
        registry
            .register(KindAction {
                id,
                title,
                built: false,
            })
            .unwrap();
    }
    registry
}

fn builtin_doc(text: &str) -> MergedDoc {
    merge_docs(
        PANELS_DOC,
        &[LayerDoc::builtin(PANELS_DOC, text).expect("well-formed test TOML")],
    )
}

/// What the loader under test accepts and reports for `text`.
fn load(text: &str) -> (Vec<PanelSpec>, Vec<Diagnostic>) {
    read_panels(&builtin_doc(text), &actions())
}

/// The one refusal `text` produces, as its path — after checking it is an
/// Error and that the refused panel is absent.
fn refused(text: &str) -> String {
    let (panels, diags) = load(text);
    assert!(panels.is_empty(), "a refused panel is absent: {panels:?}");
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0].severity, Severity::Error, "{diags:?}");
    diags[0].path.clone().expect("a refusal names its path")
}

/// `base` with its one occurrence of `from` replaced by `to`.
fn with(base: &str, from: &str, to: &str) -> String {
    assert_eq!(
        base.matches(from).count(),
        1,
        "fixture must hold {from:?} once"
    );
    base.replacen(from, to, 1)
}

fn places(precision: u8) -> ColumnFormat {
    ColumnFormat {
        precision,
        ..ColumnFormat::TEXT
    }
}

fn expected_cvi() -> PanelSpec {
    let registry = actions();
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
                format: places(2),
            },
            SliceValue {
                column: "atm".into(),
                label: "atm".into(),
                format: places(4),
            },
            SliceValue {
                column: "skew".into(),
                label: "skew".into(),
                format: places(4),
            },
        ],
        format: places(4),
        value_type: ColumnType::F64,
        actions: registry
            .ids()
            .into_iter()
            .map(|id| registry.get(id).unwrap())
            .collect(),
    }
}

#[test]
fn a_pivot_panel_reads_every_field() {
    let (panels, diags) = load(CVI_PANEL);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(panels, vec![expected_cvi()]);
}

#[test]
fn a_flat_panel_reads_every_field() {
    let (panels, diags) = load(DIVIDEND_PANEL);
    assert!(diags.is_empty(), "{diags:?}");
    let [dividend] = panels.as_slice() else {
        panic!("{panels:?}")
    };
    assert_eq!(dividend.kind, "dividend");
    assert_eq!(
        dividend.rows,
        RowAxis {
            column: "dividend_id".into(),
            identity: RowIdentity::Minted,
            label: RowLabel::Hidden,
        }
    );
    let cols = dividend.flat_columns();
    assert_eq!(
        cols.iter()
            .map(|c| (c.column.as_str(), c.label.as_str(), c.ty))
            .collect::<Vec<_>>(),
        vec![
            ("ex_date", "ex", ColumnType::Date),
            ("announced_date", "announced", ColumnType::Date),
            ("pay_date", "pay", ColumnType::Date),
            ("amount", "amount", ColumnType::F64),
            ("status", "status", ColumnType::Utf8),
        ]
    );
    assert!(cols.iter().all(|c| c.required));
    assert_eq!(
        cols[0].format,
        ColumnFormat::TEXT,
        "a date keeps the text format"
    );
    assert_eq!(cols[3].format, places(4));
    assert_eq!(
        cols[4].choices.as_deref(),
        Some(&["estimated", "declared", "paid", "cancelled"].map(String::from)[..])
    );
    assert_eq!(
        dividend.header,
        vec![
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
        ]
    );
    assert_eq!(dividend.format, places(4));
    assert_eq!(dividend.value_type, ColumnType::F64);
    assert!(dividend.actions.is_empty());
}

#[test]
fn every_format_key_overlays_the_text_default() {
    let text = with(
        CVI_PANEL,
        "format = { precision = 4 }",
        "format = { precision = 3, thousands = true, negative = \"parens\", scale = \"k\" }",
    );
    let (panels, diags) = load(&text);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        panels[0].format,
        ColumnFormat {
            precision: 3,
            thousands: true,
            negative: Negative::Parens,
            colour: Colour::None,
            scale: Scale::Thousands,
        }
    );
    let fwd = panels[0].slice_value("forward").unwrap();
    assert_eq!(
        fwd.format.scale,
        Scale::Thousands,
        "a slice's own format overlays the panel's value format"
    );
    assert_eq!(fwd.format.precision, 2);
}

#[test]
fn panels_keep_document_order_and_config_version_is_not_a_panel() {
    let (panels, diags) = load(&format!(
        "config_version = 1\n{DIVIDEND_PANEL}\n{CVI_PANEL}"
    ));
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        panels.iter().map(|p| p.kind.as_str()).collect::<Vec<_>>(),
        ["dividend", "cvi"]
    );
}

#[test]
fn a_refused_panel_leaves_its_neighbours_loaded() {
    let broken = with(CVI_PANEL, "title = \"CVI\"\n", "");
    let (panels, diags) = load(&format!("{broken}\n{DIVIDEND_PANEL}"));
    assert_eq!(
        panels.iter().map(|p| p.kind.as_str()).collect::<Vec<_>>(),
        ["dividend"]
    );
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert!(
        diags[0].message.contains("panel 'cvi'"),
        "{}",
        diags[0].message
    );
}

#[test]
fn a_panel_name_that_is_not_a_tile_kind_is_refused() {
    let text = CVI_PANEL
        .replace("[cvi]", "[Cvi2]")
        .replace("cvi.", "Cvi2.");
    assert_eq!(refused(&text), "panels.Cvi2");
}

#[test]
fn a_panel_that_is_not_a_table_is_refused() {
    assert_eq!(refused("stray = 3\n"), "panels.stray");
}

#[test]
fn an_unknown_key_is_refused() {
    let text = with(
        CVI_PANEL,
        "title = \"CVI\"",
        "title = \"CVI\"\ntilte = \"x\"",
    );
    assert_eq!(refused(&text), "panels.cvi.tilte");
}

/// The case a warning would get wrong: a misspelt `format` would paint the
/// forward at the text default's zero places.
#[test]
fn a_misspelt_format_key_is_refused_not_ignored() {
    let text = with(
        CVI_PANEL,
        "format = { precision = 2 }",
        "fromat = { precision = 2 }",
    );
    assert_eq!(refused(&text), "panels.cvi.slice.0.fromat");
}

#[test]
fn a_color_key_is_refused_because_panels_paint_the_foreground() {
    let text = with(
        CVI_PANEL,
        "format = { precision = 4 }",
        "format = { precision = 4, color = \"sign\" }",
    );
    assert_eq!(refused(&text), "panels.cvi.value.format.color");
}

#[test]
fn a_missing_required_key_is_refused() {
    assert_eq!(
        refused(&with(CVI_PANEL, "title = \"CVI\"\n", "")),
        "panels.cvi.title"
    );
    let no_required = with(
        DIVIDEND_PANEL,
        "label = \"ex\"\ntype = \"date\"\nrequired = true\n",
        "label = \"ex\"\ntype = \"date\"\n",
    );
    assert_eq!(
        refused(&no_required),
        "panels.dividend.columns.values.0.required"
    );
}

#[test]
fn a_non_numeric_value_type_is_refused() {
    let text = with(
        CVI_PANEL,
        "type = \"f64\"\nformat = { precision = 4 }",
        "type = \"date\"\nformat = { precision = 4 }",
    );
    assert_eq!(refused(&text), "panels.cvi.value.type");
}

#[test]
fn an_f64_value_without_a_precision_is_refused() {
    let no_precision = with(
        CVI_PANEL,
        "format = { precision = 4 }",
        "format = { thousands = false }",
    );
    assert_eq!(refused(&no_precision), "panels.cvi.value.format.precision");
    let no_format = with(CVI_PANEL, "\nformat = { precision = 4 }", "");
    assert_eq!(refused(&no_format), "panels.cvi.value.format");
}

#[test]
fn a_malformed_format_value_is_refused() {
    let text = with(
        CVI_PANEL,
        "format = { precision = 4 }",
        "format = { precision = 13 }",
    );
    assert_eq!(refused(&text), "panels.cvi.value.format.precision");
}

#[test]
fn columns_must_be_exactly_one_of_axis_or_values() {
    let both = with(CVI_PANEL, "axis = \"node\"", "axis = \"node\"\nvalues = []");
    assert_eq!(refused(&both), "panels.cvi.columns");
    let neither = with(CVI_PANEL, "axis = \"node\"", "");
    assert_eq!(refused(&neither), "panels.cvi.columns");
}

#[test]
fn an_unknown_row_identity_is_refused() {
    let text = with(CVI_PANEL, "identity = \"date\"", "identity = \"when\"");
    assert_eq!(refused(&text), "panels.cvi.rows.identity");
}

#[test]
fn a_hidden_label_on_a_typed_row_identity_is_refused() {
    let text = with(CVI_PANEL, "label = \"shown\"", "label = \"hidden\"");
    assert_eq!(refused(&text), "panels.cvi.rows.label");
}

#[test]
fn a_flat_panel_with_a_slice_is_refused() {
    let text =
        format!("{DIVIDEND_PANEL}\n[[dividend.slice]]\ncolumn = \"amount\"\nlabel = \"amt\"\n");
    assert_eq!(refused(&text), "panels.dividend.slice");
}

#[test]
fn choices_on_a_non_text_column_are_refused() {
    let text = with(
        DIVIDEND_PANEL,
        "label = \"ex\"\ntype = \"date\"",
        "label = \"ex\"\ntype = \"date\"\nchoices = [\"a\"]",
    );
    assert_eq!(refused(&text), "panels.dividend.columns.values.0.choices");
}

#[test]
fn empty_choices_are_refused() {
    let text = with(
        DIVIDEND_PANEL,
        "choices = [\"estimated\", \"declared\", \"paid\", \"cancelled\"]",
        "choices = []",
    );
    assert_eq!(refused(&text), "panels.dividend.columns.values.4.choices");
}

#[test]
fn a_format_on_a_date_column_is_refused() {
    let text = with(
        DIVIDEND_PANEL,
        "label = \"ex\"\ntype = \"date\"",
        "label = \"ex\"\ntype = \"date\"\nformat = { precision = 2 }",
    );
    assert_eq!(refused(&text), "panels.dividend.columns.values.0.format");
}

#[test]
fn two_value_columns_sharing_a_label_are_refused() {
    let text = with(DIVIDEND_PANEL, "label = \"announced\"", "label = \"ex\"");
    assert_eq!(refused(&text), "panels.dividend.columns.values.1.label");
}

#[test]
fn two_slices_sharing_a_label_are_refused() {
    let text = with(CVI_PANEL, "label = \"skew\"", "label = \"atm\"");
    assert_eq!(refused(&text), "panels.cvi.slice.2.label");
}

#[test]
fn a_column_named_twice_is_refused() {
    let text = with(CVI_PANEL, "column = \"spot_ref\"", "column = \"forward\"");
    assert_eq!(refused(&text), "panels.cvi.slice.0.column");
}

#[test]
fn an_unregistered_kind_action_is_refused() {
    let text = with(
        CVI_PANEL,
        "\"marketdata::cvi_recalc_forward\"]",
        "\"marketdata::cvi_nonesuch\"]",
    );
    assert_eq!(refused(&text), "panels.cvi.actions.1");
}

fn layer_doc(layer: Layer, text: &str) -> LayerDoc {
    LayerDoc {
        layer,
        name: PANELS_DOC.into(),
        file: format!("{}/panels.toml", layer.name()).into(),
        table: text.parse().expect("well-formed test TOML"),
    }
}

/// Named objects replace whole: a desk panel of the same name is the panel.
#[test]
fn a_desk_panel_replaces_the_builtin_one_whole() {
    let desk = with(CVI_PANEL, "title = \"CVI\"", "title = \"Desk CVI\"");
    let doc = merge_docs(
        PANELS_DOC,
        &[
            LayerDoc::builtin(PANELS_DOC, CVI_PANEL).unwrap(),
            layer_doc(Layer::Desk, &desk),
        ],
    );
    let (panels, diags) = read_panels(&doc, &actions());
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(panels[0].title, "Desk CVI");
}

/// A partial user override does not inherit the builtin's columns: it is the
/// whole panel, and holding only a title it is refused, attributed to the
/// user layer.
#[test]
fn a_user_panel_replaces_the_builtin_one_whole() {
    let doc = merge_docs(
        PANELS_DOC,
        &[
            LayerDoc::builtin(PANELS_DOC, CVI_PANEL).unwrap(),
            layer_doc(Layer::User, "[cvi]\ntitle = \"Mine\"\n"),
        ],
    );
    let (panels, diags) = read_panels(&doc, &actions());
    assert!(panels.is_empty(), "{panels:?}");
    assert_eq!(diags[0].path.as_deref(), Some("panels.cvi.dataset"));
    assert_eq!(diags[0].layer, Some(Layer::User));
}
