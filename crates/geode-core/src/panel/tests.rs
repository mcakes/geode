use super::*;
use crate::config::{Diagnostic, Layer, LayerDoc, MergedDoc, Severity, merge_docs};
use crate::document::{DocumentKind, DocumentRows, ParseError, ParsedDocument, WriteError};
use crate::schema::{ColumnType, SchemaSpec};
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

/// `cvi_params` and `dividend_schedule` as the demo declares them, a
/// text-axis pivot (`tenor_grid`), an integer-axis flat set
/// (`strike_ladder`) and an integer ladder with an f64 slice (`count_grid`).
const DATASETS: &str = r#"
[cvi_params]
family = "document"
key = ["underlying_ref"]
axes = ["term", "node"]
[cvi_params.columns.underlying_ref]
type = "utf8"
role = "dimension"
[cvi_params.columns.term]
type = "date"
role = "axis"
[cvi_params.columns.node]
type = "f64"
role = "axis"
[cvi_params.columns.param]
type = "f64"
role = "value"
[cvi_params.columns.forward]
type = "f64"
role = "value"
[cvi_params.columns.atm]
type = "f64"
role = "value"
[cvi_params.columns.skew]
type = "f64"
role = "value"
[cvi_params.columns.anchor_date]
type = "date"
role = "attribute"
[cvi_params.columns.spot_ref]
type = "f64"
role = "attribute"

[dividend_schedule]
family = "document"
key = ["underlying_ref"]
axes = ["dividend_id"]
[dividend_schedule.columns.underlying_ref]
type = "utf8"
role = "dimension"
[dividend_schedule.columns.dividend_id]
type = "utf8"
role = "axis"
[dividend_schedule.columns.ex_date]
type = "date"
role = "value"
[dividend_schedule.columns.announced_date]
type = "date"
role = "value"
[dividend_schedule.columns.pay_date]
type = "date"
role = "value"
[dividend_schedule.columns.amount]
type = "f64"
role = "value"
[dividend_schedule.columns.status]
type = "utf8"
role = "value"
[dividend_schedule.columns.currency]
type = "utf8"
role = "attribute"
[dividend_schedule.columns.schedule_date]
type = "date"
role = "attribute"

[tenor_grid]
family = "document"
key = ["underlying_ref"]
axes = ["tenor", "bucket"]
[tenor_grid.columns.underlying_ref]
type = "utf8"
role = "dimension"
[tenor_grid.columns.tenor]
type = "utf8"
role = "axis"
[tenor_grid.columns.bucket]
type = "utf8"
role = "axis"
[tenor_grid.columns.vol]
type = "f64"
role = "value"
[tenor_grid.columns.fwd]
type = "f64"
role = "value"

[strike_ladder]
family = "document"
key = ["underlying_ref"]
axes = ["strike"]
[strike_ladder.columns.underlying_ref]
type = "utf8"
role = "dimension"
[strike_ladder.columns.strike]
type = "i64"
role = "axis"
[strike_ladder.columns.vol]
type = "f64"
role = "value"

[count_grid]
family = "document"
key = ["underlying_ref"]
axes = ["term", "node"]
[count_grid.columns.underlying_ref]
type = "utf8"
role = "dimension"
[count_grid.columns.term]
type = "date"
role = "axis"
[count_grid.columns.node]
type = "f64"
role = "axis"
[count_grid.columns.count]
type = "i64"
role = "value"
[count_grid.columns.fwd]
type = "f64"
role = "value"
"#;

const GRID_PANEL: &str = r#"
[grid]
title = "Grid"
dataset = "tenor_grid"
document = "tenor_grid"
[grid.value]
type = "f64"
format = { precision = 4 }
[grid.rows]
column = "tenor"
identity = "utf8"
label = "shown"
[grid.columns]
axis = "bucket"
[[grid.slice]]
column = "fwd"
label = "fwd"
"#;

const LADDER_PANEL: &str = r#"
[ladder]
title = "Ladder"
dataset = "strike_ladder"
document = "strike_ladder"
[ladder.value]
type = "f64"
format = { precision = 4 }
[ladder.rows]
column = "strike"
identity = "i64"
label = "shown"
[[ladder.columns.values]]
column = "vol"
label = "vol"
type = "f64"
format = { precision = 4 }
required = true
"#;

/// An integer ladder with an f64 slice: the slice's editor would parse the
/// forward as the panel's i64.
const COUNT_PANEL: &str = r#"
[counts]
title = "Counts"
dataset = "count_grid"
document = "count_grid"
[counts.value]
type = "i64"
[counts.rows]
column = "term"
identity = "date"
label = "shown"
[counts.columns]
axis = "node"
[[counts.slice]]
column = "fwd"
label = "fwd"
format = { precision = 2 }
"#;

fn schema() -> SchemaSpec {
    let doc = merge_docs(
        "datasets",
        &[LayerDoc::builtin("datasets", DATASETS).unwrap()],
    );
    let (schema, diags) = SchemaSpec::from_doc(&doc);
    assert!(diags.is_empty(), "{diags:?}");
    schema
}

struct TestKind {
    name: &'static str,
    columns: Vec<(&'static str, ColumnType)>,
}

impl DocumentKind for TestKind {
    fn name(&self) -> &'static str {
        self.name
    }
    fn columns(&self) -> &[(&'static str, ColumnType)] {
        &self.columns
    }
    fn parse(&self, _bytes: &[u8]) -> Result<ParsedDocument, ParseError> {
        Err(ParseError {
            message: "a test kind does not parse".into(),
        })
    }
    fn write(&self, _rows: &DocumentRows) -> Result<Vec<u8>, WriteError> {
        Err(WriteError {
            message: "a test kind does not write".into(),
        })
    }
}

/// A kind named after `dataset` producing its document columns, less `drop`.
fn kind(schema: &SchemaSpec, dataset: &'static str, drop: &[&str]) -> Arc<dyn DocumentKind> {
    let columns = schema
        .dataset(dataset)
        .unwrap()
        .document_columns()
        .into_iter()
        .filter(|c| !drop.contains(&c.name.as_str()))
        // Leaked: `DocumentKind::columns` hands out `&'static str` names.
        .map(|c| (&*Box::leak(c.name.clone().into_boxed_str()), c.ty))
        .collect();
    Arc::new(TestKind {
        name: dataset,
        columns,
    })
}

fn kinds() -> Vec<Arc<dyn DocumentKind>> {
    let s = schema();
    [
        "cvi_params",
        "dividend_schedule",
        "tenor_grid",
        "strike_ladder",
        "count_grid",
    ]
    .into_iter()
    .map(|d| kind(&s, d, &[]))
    .collect()
}

/// What the loader under test accepts and reports for `text`: both halves,
/// so every structural test also shows the cross-document half passes it.
fn load(text: &str) -> (Vec<PanelSpec>, Vec<Diagnostic>) {
    load_panels(&builtin_doc(text), &actions(), &schema(), &kinds())
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

#[test]
fn the_shipped_shapes_and_an_integer_axis_load_clean() {
    let (panels, diags) = load(&format!("{CVI_PANEL}\n{DIVIDEND_PANEL}\n{LADDER_PANEL}"));
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(panels.len(), 3);
}

/// The builtin layer gets no exemption: a desk without the market-data
/// datasets sees the shipped panels refused by name.
#[test]
fn an_undeclared_dataset_is_refused() {
    let text = with(
        CVI_PANEL,
        "dataset = \"cvi_params\"",
        "dataset = \"nonesuch\"",
    );
    assert_eq!(refused(&text), "panels.cvi.dataset");
}

#[test]
fn an_unregistered_document_kind_is_refused() {
    let text = with(
        CVI_PANEL,
        "document = \"cvi_params\"",
        "document = \"vol_xml\"",
    );
    assert_eq!(refused(&text), "panels.cvi.document");
}

#[test]
fn a_document_kind_that_does_not_fit_the_dataset_is_refused() {
    let s = schema();
    let short = vec![kind(&s, "cvi_params", &["skew"])];
    let (panels, diags) = load_panels(&builtin_doc(CVI_PANEL), &actions(), &s, &short);
    assert!(panels.is_empty(), "{panels:?}");
    assert_eq!(diags[0].path.as_deref(), Some("panels.cvi.document"));
}

#[test]
fn a_row_axis_that_is_not_the_first_axis_is_refused() {
    let text = with(CVI_PANEL, "column = \"term\"", "column = \"node\"").replacen(
        "axis = \"node\"",
        "axis = \"term\"",
        1,
    );
    assert_eq!(refused(&text), "panels.cvi.rows.column");
}

#[test]
fn a_pivot_axis_the_dataset_does_not_have_is_refused() {
    let text = with(CVI_PANEL, "axis = \"node\"", "axis = \"param\"");
    assert_eq!(refused(&text), "panels.cvi.columns.axis");
}

#[test]
fn a_row_identity_the_dataset_contradicts_is_refused() {
    let text = with(CVI_PANEL, "identity = \"date\"", "identity = \"f64\"");
    assert_eq!(refused(&text), "panels.cvi.rows.identity");
}

#[test]
fn a_minted_identity_over_a_non_text_axis_is_refused() {
    let text = with(LADDER_PANEL, "identity = \"i64\"", "identity = \"minted\"");
    assert_eq!(refused(&text), "panels.ladder.rows.identity");
}

#[test]
fn a_header_column_the_dataset_lacks_is_refused() {
    let text = with(CVI_PANEL, "column = \"spot_ref\"", "column = \"spot\"");
    assert_eq!(refused(&text), "panels.cvi.header.1.column");
}

#[test]
fn a_header_type_the_dataset_contradicts_is_refused() {
    let text = with(
        CVI_PANEL,
        "label = \"spot\"\ntype = \"f64\"",
        "label = \"spot\"\ntype = \"date\"",
    );
    assert_eq!(refused(&text), "panels.cvi.header.1.type");
}

#[test]
fn a_header_column_that_is_not_an_attribute_is_refused() {
    let text = with(CVI_PANEL, "column = \"anchor_date\"", "column = \"param\"");
    assert_eq!(refused(&text), "panels.cvi.header.0.column");
}

#[test]
fn a_flat_column_the_dataset_lacks_is_refused() {
    let text = with(
        DIVIDEND_PANEL,
        "column = \"pay_date\"",
        "column = \"paid_date\"",
    );
    assert_eq!(refused(&text), "panels.dividend.columns.values.2.column");
}

#[test]
fn a_flat_column_type_the_dataset_contradicts_is_refused() {
    let text = with(
        DIVIDEND_PANEL,
        "label = \"amount\"\ntype = \"f64\"\nformat = { precision = 4 }",
        "label = \"amount\"\ntype = \"i64\"",
    );
    assert_eq!(refused(&text), "panels.dividend.columns.values.3.type");
}

#[test]
fn a_flat_column_that_is_not_a_value_is_refused() {
    let text = with(
        DIVIDEND_PANEL,
        "column = \"ex_date\"",
        "column = \"underlying_ref\"",
    );
    assert_eq!(refused(&text), "panels.dividend.columns.values.0.column");
}

#[test]
fn a_slice_that_is_not_a_value_is_refused() {
    let text = with(CVI_PANEL, "column = \"atm\"", "column = \"underlying_ref\"");
    assert_eq!(refused(&text), "panels.cvi.slice.1.column");
}

/// Dropping a slice leaves two unnamed values; the model would refuse the
/// pivot on every delivery, so the panel is refused at load instead.
#[test]
fn a_pivot_that_leaves_two_value_columns_is_refused() {
    let text = with(
        CVI_PANEL,
        "\n[[cvi.slice]]\ncolumn = \"skew\"\nlabel = \"skew\"\n",
        "\n",
    );
    assert_eq!(refused(&text), "panels.cvi.slice");
}

#[test]
fn a_ladder_value_type_the_dataset_contradicts_is_refused() {
    let text = with(
        CVI_PANEL,
        "type = \"f64\"\nformat = { precision = 4 }",
        "type = \"i64\"\nformat = { precision = 4 }",
    );
    assert_eq!(refused(&text), "panels.cvi.value.type");
}

/// Slice editors parse by the panel's value type: an i64 panel over an f64
/// forward would refuse a fractional edit.
#[test]
fn a_slice_whose_type_differs_from_the_value_type_is_refused() {
    assert_eq!(refused(COUNT_PANEL), "panels.counts.slice.0.column");
    let (_, diags) = load(COUNT_PANEL);
    assert!(
        diags[0].message.contains("value.type is i64"),
        "{}",
        diags[0].message
    );
}

#[test]
fn a_slice_label_a_numeric_axis_could_produce_is_refused() {
    let text = with(CVI_PANEL, "label = \"fwd\"", "label = \"10\"");
    assert_eq!(refused(&text), "panels.cvi.slice.0.label");
}

/// A text axis's labels come from the data, so no slice label can be
/// proven distinct from them at load.
#[test]
fn a_slice_over_a_text_axis_is_refused() {
    assert_eq!(refused(GRID_PANEL), "panels.grid.slice.0.label");
}

/// A panel that leaves a written column unnamed could never upload a whole
/// document: refused, naming what it leaves out.
#[test]
fn a_flat_panel_missing_a_value_column_is_refused() {
    let text = with(
        DIVIDEND_PANEL,
        "[[dividend.columns.values]]\ncolumn = \"status\"\nlabel = \"status\"\ntype = \"utf8\"\nchoices = [\"estimated\", \"declared\", \"paid\", \"cancelled\"]\nrequired = true\n",
        "",
    );
    assert_eq!(refused(&text), "panels.dividend.columns.values");
    let (_, diags) = load(&text);
    assert!(
        diags[0].message.contains("[status]"),
        "{}",
        diags[0].message
    );
}

#[test]
fn a_panel_missing_a_header_attribute_is_refused() {
    let text = with(
        CVI_PANEL,
        "[[cvi.header]]\ncolumn = \"spot_ref\"\nlabel = \"spot\"\ntype = \"f64\"\n",
        "",
    );
    assert_eq!(refused(&text), "panels.cvi.header");
    let (_, diags) = load(&text);
    assert!(
        diags[0].message.contains("[spot_ref]"),
        "{}",
        diags[0].message
    );
}
