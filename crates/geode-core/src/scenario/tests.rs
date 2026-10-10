use super::*;
use crate::config::{Layer, LayerDoc, MergedDoc, merge_docs};
use crate::view::{Colour, Scale};

pub(super) fn builtin_doc(text: &str) -> MergedDoc {
    merge_docs(
        SCENARIOS_DOC,
        &[LayerDoc::builtin(SCENARIOS_DOC, text).expect("well-formed test TOML")],
    )
}

pub(super) fn layer_doc(layer: Layer, text: &str) -> LayerDoc {
    LayerDoc {
        layer,
        name: SCENARIOS_DOC.into(),
        file: format!("{}/scenarios.toml", layer.name()).into(),
        table: text.parse().expect("well-formed test TOML"),
    }
}

pub(super) const VEGA_KT: &str = r#"
[vega_kt]
title = "Vega K/T"
dataset = "vega_buckets"
rows = "tenor"
columns = { axis = "moneyness" }
value = "vega"
paint = "heat"
"#;

pub(super) const SPOT_LADDER: &str = r#"
[spot_ladder]
title = "Spot ladder"
dataset = "spot_ladder"
rows = "spot_shift"
columns = { measures = [
  { column = "npv", label = "P&L vs base" },
  { column = "delta", label = "Δ01", format = { precision = 0, scale = "k" } },
  { column = "theta", unit = "per day" },
] }
diff = { base = 0, measures = ["npv"] }
paint = "heat"
"#;

/// `text` with `from` replaced by `to`, asserting it was there to replace.
pub(super) fn with(text: &str, from: &str, to: &str) -> String {
    assert!(text.contains(from), "fixture lacks {from:?}");
    text.replace(from, to)
}

/// The path of the one refusal `text` produces under `read_scenarios`.
fn refused(text: &str) -> String {
    let (shapes, diags) = read_scenarios(&builtin_doc(text));
    assert!(shapes.is_empty(), "accepted: {shapes:?}");
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0].severity, crate::config::Severity::Error);
    diags[0].path.clone().expect("a refusal carries its path")
}

#[test]
fn a_matrix_shape_reads_its_axis_columns_and_value() {
    let (shapes, diags) = read_scenarios(&builtin_doc(VEGA_KT));
    assert!(diags.is_empty(), "{diags:?}");
    let s = &shapes[0];
    assert_eq!(s.kind, "vega_kt");
    assert_eq!(s.title, "Vega K/T");
    assert_eq!(s.dataset, "vega_buckets");
    assert_eq!(s.rows, "tenor");
    assert_eq!(s.columns, Columns::Axes(vec!["moneyness".into()]));
    let value = s.value.as_ref().unwrap();
    assert_eq!(value.column, "vega");
    assert_eq!(value.label, "vega", "label defaults to the column");
    assert_eq!(value.format, ColumnFormat::MEASURE);
    assert_eq!(value.unit, None);
    assert_eq!(value.usd_twin, None, "twins are the checker's");
    assert_eq!(s.paint, Paint::Heat);
    assert!(s.axes.is_empty(), "axis facts are the checker's");
    assert_eq!(s.grouping(), ["tenor", "moneyness"]);
    assert!(
        !s.sums_rows() && !s.sums_columns(),
        "no Σ before the axes are known"
    );
}

#[test]
fn a_one_element_axis_array_reads_as_one_level() {
    let text = with(
        VEGA_KT,
        r#"{ axis = "moneyness" }"#,
        r#"{ axis = ["moneyness"] }"#,
    );
    let (shapes, diags) = read_scenarios(&builtin_doc(&text));
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(shapes[0].columns, Columns::Axes(vec!["moneyness".into()]));
}

#[test]
fn two_column_axes_keep_their_order_outer_first() {
    let text = with(
        VEGA_KT,
        r#"{ axis = "moneyness" }"#,
        r#"{ axis = ["curve_ccy", "curve_type"] }"#,
    );
    let (shapes, _) = read_scenarios(&builtin_doc(&text));
    assert_eq!(
        shapes[0].columns,
        Columns::Axes(vec!["curve_ccy".into(), "curve_type".into()])
    );
    assert_eq!(shapes[0].grouping(), ["tenor", "curve_ccy", "curve_type"]);
}

#[test]
fn a_ladder_shape_reads_its_measures_with_labels_formats_and_units() {
    let (shapes, diags) = read_scenarios(&builtin_doc(SPOT_LADDER));
    assert!(diags.is_empty(), "{diags:?}");
    let s = &shapes[0];
    assert!(s.value.is_none());
    let Columns::Measures(cols) = &s.columns else {
        panic!("{:?}", s.columns)
    };
    assert_eq!(cols.len(), 3);
    assert_eq!(cols[0].label, "P&L vs base");
    assert_eq!(cols[1].label, "Δ01");
    assert_eq!(cols[1].format.precision, 0);
    assert_eq!(cols[1].format.scale, Scale::Thousands);
    assert_eq!(
        cols[1].format.colour,
        Colour::Sign,
        "the measure base carries sign color"
    );
    assert_eq!(cols[2].label, "theta", "label defaults to the column");
    assert_eq!(cols[2].unit.as_deref(), Some("per day"));
    assert_eq!(
        s.diff,
        Some(Diff {
            base: DiffBase::Number(0.0),
            measures: vec!["npv".into()],
        })
    );
    assert_eq!(s.grouping(), ["spot_shift"]);
    assert_eq!(
        s.totals_grouping(),
        None,
        "a ladder never issues a second query"
    );
}

#[test]
fn a_text_diff_base_reads_as_text() {
    let text = with(SPOT_LADDER, "base = 0", r#"base = "spot""#);
    let (shapes, _) = read_scenarios(&builtin_doc(&text));
    assert_eq!(
        shapes[0].diff.as_ref().unwrap().base,
        DiffBase::Text("spot".into())
    );
}

#[test]
fn the_name_must_be_a_usable_kind_without_a_placement_suffix() {
    assert_eq!(
        refused(&with(VEGA_KT, "[vega_kt]", "[Vega-KT]")),
        "scenarios.Vega-KT"
    );
    assert_eq!(
        refused(&with(VEGA_KT, "[vega_kt]", "[vega_stacked]")),
        "scenarios.vega_stacked"
    );
}

#[test]
fn structural_refusals_name_their_field() {
    for (from, to, at) in [
        ("title = \"Vega K/T\"\n", "", "scenarios.vega_kt.title"),
        (
            "title = \"Vega K/T\"",
            "title = \"Vega K/T\"\nextra = 1",
            "scenarios.vega_kt.extra",
        ),
        ("rows = \"tenor\"\n", "", "scenarios.vega_kt.rows"),
        (
            "columns = { axis = \"moneyness\" }",
            "columns = { }",
            "scenarios.vega_kt.columns",
        ),
        (
            "columns = { axis = \"moneyness\" }",
            "columns = { axis = \"moneyness\", measures = [] }",
            "scenarios.vega_kt.columns",
        ),
        (
            "columns = { axis = \"moneyness\" }",
            "columns = { axis = [\"a\", \"b\", \"c\"] }",
            "scenarios.vega_kt.columns.axis",
        ),
        (
            "columns = { axis = \"moneyness\" }",
            "columns = { axis = [\"moneyness\", \"moneyness\"] }",
            "scenarios.vega_kt.columns.axis",
        ),
        (
            "columns = { axis = \"moneyness\" }",
            "columns = { axis = \"tenor\" }",
            "scenarios.vega_kt.columns.axis",
        ),
        ("value = \"vega\"\n", "", "scenarios.vega_kt.value"),
        (
            "paint = \"heat\"",
            "paint = \"loud\"",
            "scenarios.vega_kt.paint",
        ),
        ("paint = \"heat\"\n", "", "scenarios.vega_kt.paint"),
        (
            "paint = \"heat\"",
            "paint = \"heat\"\ndiff = { base = 0, measures = [\"vega\"] }",
            "scenarios.vega_kt.diff",
        ),
        (
            "value = \"vega\"",
            "value = \"vega\"\nformat = { precision = 99 }",
            "scenarios.vega_kt.format.precision",
        ),
        (
            "value = \"vega\"",
            "value = \"vega\"\nformat = { colour = \"sign\" }",
            "scenarios.vega_kt.format.colour",
        ),
    ] {
        assert_eq!(refused(&with(VEGA_KT, from, to)), at, "{from:?} → {to:?}");
    }
}

#[test]
fn ladder_refusals_name_their_field() {
    for (from, to, at) in [
        (
            "measures = [\n",
            "measures = [\n  \"npv\",\n",
            "scenarios.spot_ladder.columns.measures.0",
        ),
        (
            "{ column = \"theta\", unit = \"per day\" },",
            "{ column = \"theta\", unit = \"per day\" },\n  { column = \"npv\" },",
            "scenarios.spot_ladder.columns.measures.3.column",
        ),
        (
            "{ column = \"theta\", unit = \"per day\" },",
            "{ column = \"theta\", label = \"P&L vs base\" },",
            "scenarios.spot_ladder.columns.measures.2.label",
        ),
        (
            "{ column = \"theta\", unit = \"per day\" },",
            "{ column = \"theta\", colour = \"sign\" },",
            "scenarios.spot_ladder.columns.measures.2.colour",
        ),
        (
            "paint = \"heat\"",
            "paint = \"heat\"\nvalue = \"npv\"",
            "scenarios.spot_ladder.value",
        ),
        (
            "paint = \"heat\"",
            "paint = \"heat\"\nunit = \"bp\"",
            "scenarios.spot_ladder.unit",
        ),
        (
            "measures = [\"npv\"]",
            "measures = [\"vega\"]",
            "scenarios.spot_ladder.diff.measures.0",
        ),
        (
            "measures = [\"npv\"]",
            "measures = []",
            "scenarios.spot_ladder.diff.measures",
        ),
        ("base = 0", "base = true", "scenarios.spot_ladder.diff.base"),
        (
            "diff = { base = 0, measures = [\"npv\"] }",
            "diff = { measures = [\"npv\"] }",
            "scenarios.spot_ladder.diff.base",
        ),
        (
            "columns = { measures = [",
            "columns = { measures = [] }\nold = { measures = [",
            "scenarios.spot_ladder.old",
        ),
    ] {
        assert_eq!(
            refused(&with(SPOT_LADDER, from, to)),
            at,
            "{from:?} → {to:?}"
        );
    }
}

#[test]
fn a_refused_entry_leaves_its_neighbours_standing() {
    let text = format!(
        "{VEGA_KT}\n{}",
        with(SPOT_LADDER, "paint = \"heat\"", "paint = \"x\"")
    );
    let (shapes, diags) = read_scenarios(&builtin_doc(&text));
    assert_eq!(shapes.len(), 1);
    assert_eq!(shapes[0].kind, "vega_kt");
    assert_eq!(
        diags[0].path.as_deref(),
        Some("scenarios.spot_ladder.paint")
    );
}

#[test]
fn config_version_is_not_a_shape() {
    let (shapes, diags) = read_scenarios(&builtin_doc(&format!("config_version = 1\n{VEGA_KT}")));
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(shapes.len(), 1);
}

/// Named objects replace whole: a desk shape of the same name is the shape.
#[test]
fn a_desk_shape_replaces_the_builtin_one_whole() {
    let desk = with(VEGA_KT, "title = \"Vega K/T\"", "title = \"Desk vega\"");
    let doc = merge_docs(
        SCENARIOS_DOC,
        &[
            LayerDoc::builtin(SCENARIOS_DOC, VEGA_KT).unwrap(),
            layer_doc(Layer::Desk, &desk),
        ],
    );
    let (shapes, diags) = read_scenarios(&doc);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(shapes[0].title, "Desk vega");
}

/// A partial user override inherits nothing: holding only a title it is
/// refused, attributed to the user layer, and the builtin it replaced is
/// gone too (d8).
#[test]
fn a_user_shape_replaces_the_builtin_one_whole_and_a_refused_one_takes_it_down() {
    let doc = merge_docs(
        SCENARIOS_DOC,
        &[
            LayerDoc::builtin(SCENARIOS_DOC, VEGA_KT).unwrap(),
            layer_doc(Layer::User, "[vega_kt]\ntitle = \"Mine\"\n"),
        ],
    );
    let (shapes, diags) = read_scenarios(&doc);
    assert!(shapes.is_empty(), "{shapes:?}");
    assert_eq!(diags[0].path.as_deref(), Some("scenarios.vega_kt.dataset"));
    assert_eq!(diags[0].layer, Some(Layer::User));
    assert!(diags[0].message.contains("refused"));
}

use crate::schema::axis::{AxisKind, AxisOrder};
use crate::schema::{ColumnType, SchemaSpec};

/// `vega_buckets` (two bucket axes, a tenor-ordered text axis and an f64
/// one, vega with a twin), `spot_ladder` (one scenario axis, measures with
/// twins and one without), `rho_curves` (three bucket axes) and an axis-free
/// measures dataset, as the demo declares them in miniature. Each carries
/// the grain key columns the schema validator requires and nothing more.
pub(super) const DATASETS: &str = r#"
[vega_buckets]
axes = ["tenor", "moneyness"]
[vega_buckets.columns.position_ref]
type = "utf8"
role = "key"
[vega_buckets.columns.lhu]
type = "utf8"
role = "dimension"
[vega_buckets.columns.counterparty]
type = "utf8"
role = "dimension"
[vega_buckets.columns.instrument_ref]
type = "utf8"
role = "key"
[vega_buckets.columns.underlying_ref]
type = "utf8"
role = "dimension"
[vega_buckets.columns.book]
type = "utf8"
role = "dimension"
[vega_buckets.columns.tenor]
type = "utf8"
role = "axis"
axis = "bucket"
order = "tenor"
[vega_buckets.columns.moneyness]
type = "f64"
role = "axis"
axis = "bucket"
[vega_buckets.columns.vega]
type = "f64"
role = "measure"
grain = "underlying"
[vega_buckets.columns.vega_usd]
type = "f64"
role = "measure"
grain = "underlying"

[spot_ladder]
axes = ["spot_shift"]
[spot_ladder.columns.position_ref]
type = "utf8"
role = "key"
[spot_ladder.columns.book]
type = "utf8"
role = "dimension"
[spot_ladder.columns.lhu]
type = "utf8"
role = "dimension"
[spot_ladder.columns.counterparty]
type = "utf8"
role = "dimension"
[spot_ladder.columns.instrument_ref]
type = "utf8"
role = "key"
[spot_ladder.columns.underlying_ref]
type = "utf8"
role = "dimension"
[spot_ladder.columns.spot_shift]
type = "f64"
role = "axis"
axis = "scenario"
[spot_ladder.columns.npv]
type = "f64"
role = "measure"
grain = "instrument"
[spot_ladder.columns.npv_usd]
type = "f64"
role = "measure"
grain = "instrument"
[spot_ladder.columns.delta]
type = "f64"
role = "measure"
grain = "underlying"
[spot_ladder.columns.delta_usd]
type = "f64"
role = "measure"
grain = "underlying"
[spot_ladder.columns.theta]
type = "f64"
role = "measure"
grain = "instrument"

[rho_curves]
axes = ["tenor", "curve_ccy", "curve_type"]
[rho_curves.columns.position_ref]
type = "utf8"
role = "key"
[rho_curves.columns.book]
type = "utf8"
role = "dimension"
[rho_curves.columns.lhu]
type = "utf8"
role = "dimension"
[rho_curves.columns.counterparty]
type = "utf8"
role = "dimension"
[rho_curves.columns.instrument_ref]
type = "utf8"
role = "key"
[rho_curves.columns.underlying_ref]
type = "utf8"
role = "dimension"
[rho_curves.columns.tenor]
type = "utf8"
role = "axis"
axis = "bucket"
order = "tenor"
[rho_curves.columns.curve_ccy]
type = "utf8"
role = "axis"
axis = "bucket"
[rho_curves.columns.curve_type]
type = "utf8"
role = "axis"
axis = "bucket"
[rho_curves.columns.rho]
type = "f64"
role = "measure"
grain = "underlying"

[risk_snapshot]
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.lhu]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.counterparty]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.instrument_ref]
type = "utf8"
role = "key"
[risk_snapshot.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.vega]
type = "f64"
role = "measure"
grain = "underlying"
"#;

pub(super) fn schema() -> SchemaSpec {
    let doc = merge_docs(
        "datasets",
        &[LayerDoc::builtin("datasets", DATASETS).expect("well-formed test TOML")],
    );
    let (schema, diags) = SchemaSpec::from_doc(&doc);
    assert!(diags.is_empty(), "{diags:?}");
    schema
}

fn load(text: &str) -> (Vec<ShapeSpec>, Vec<Diagnostic>) {
    load_scenarios(&builtin_doc(text), &schema())
}

/// The path of the one refusal `text` produces under `load_scenarios`.
fn refused_by_check(text: &str) -> String {
    let (shapes, diags) = load(text);
    assert!(shapes.is_empty(), "accepted: {shapes:?}");
    assert_eq!(diags.len(), 1, "{diags:?}");
    diags[0].path.clone().expect("a refusal carries its path")
}

pub(super) const RHO: &str = r#"
[rho]
title = "Rho"
dataset = "rho_curves"
rows = "tenor"
columns = { axis = ["curve_ccy", "curve_type"] }
value = "rho"
unit = "per 10 bp"
paint = "heat"
"#;

#[test]
fn a_checked_matrix_carries_its_axes_in_dataset_order_and_its_twin() {
    let (shapes, diags) = load(VEGA_KT);
    assert!(diags.is_empty(), "{diags:?}");
    let s = &shapes[0];
    assert_eq!(
        s.axes,
        vec![
            AxisInfo {
                name: "tenor".into(),
                kind: AxisKind::Bucket,
                order: AxisOrder::Tenor,
                ty: ColumnType::Utf8,
            },
            AxisInfo {
                name: "moneyness".into(),
                kind: AxisKind::Bucket,
                order: AxisOrder::Lexical,
                ty: ColumnType::F64,
            },
        ]
    );
    assert_eq!(
        s.value.as_ref().unwrap().usd_twin.as_deref(),
        Some("vega_usd")
    );
    assert!(s.sums_rows() && s.sums_columns());
    assert_eq!(s.totals_grouping(), Some(vec!["moneyness".into()]));
    assert_eq!(s.query_measures(), ["vega", "vega_usd"]);
}

#[test]
fn a_checked_ladder_fills_twins_where_declared_and_leaves_the_rest() {
    let (shapes, diags) = load(SPOT_LADDER);
    assert!(diags.is_empty(), "{diags:?}");
    let s = &shapes[0];
    let Columns::Measures(cols) = &s.columns else {
        unreachable!()
    };
    assert_eq!(cols[0].usd_twin.as_deref(), Some("npv_usd"));
    assert_eq!(cols[1].usd_twin.as_deref(), Some("delta_usd"));
    assert_eq!(cols[2].usd_twin, None, "theta has no twin");
    assert_eq!(s.axes[0].kind, AxisKind::Scenario);
    assert!(!s.sums_rows(), "no Σ across spot shifts");
    assert!(!s.sums_columns(), "never across measures");
    assert_eq!(
        s.query_measures(),
        ["npv", "npv_usd", "delta", "delta_usd", "theta"]
    );
}

#[test]
fn a_two_level_matrix_sums_both_ways_over_bucket_axes() {
    let (shapes, diags) = load(RHO);
    assert!(diags.is_empty(), "{diags:?}");
    let s = &shapes[0];
    assert!(s.sums_rows() && s.sums_columns());
    assert_eq!(
        s.totals_grouping(),
        Some(vec!["curve_ccy".into(), "curve_type".into()])
    );
    assert_eq!(s.value.as_ref().unwrap().unit.as_deref(), Some("per 10 bp"));
    assert_eq!(s.value.as_ref().unwrap().usd_twin, None);
}

#[test]
fn a_matrix_over_a_scenario_columns_axis_has_no_column_sum() {
    // vega by spot shift: rows tenor (bucket), columns spot_shift (scenario).
    let datasets = format!(
        "{DATASETS}\n[vega_spot]\naxes = [\"tenor\", \"spot_shift\"]\n\
         [vega_spot.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
         [vega_spot.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [vega_spot.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [vega_spot.columns.counterparty]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [vega_spot.columns.instrument_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
         [vega_spot.columns.underlying_ref]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [vega_spot.columns.tenor]\ntype = \"utf8\"\nrole = \"axis\"\naxis = \"bucket\"\norder = \"tenor\"\n\
         [vega_spot.columns.spot_shift]\ntype = \"f64\"\nrole = \"axis\"\naxis = \"scenario\"\n\
         [vega_spot.columns.vega]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"underlying\"\n"
    );
    let doc = merge_docs(
        "datasets",
        &[LayerDoc::builtin("datasets", &datasets).unwrap()],
    );
    let (schema, diags) = SchemaSpec::from_doc(&doc);
    assert!(diags.is_empty(), "{diags:?}");
    let text = with(
        &with(
            VEGA_KT,
            "dataset = \"vega_buckets\"",
            "dataset = \"vega_spot\"",
        ),
        r#"{ axis = "moneyness" }"#,
        r#"{ axis = "spot_shift" }"#,
    );
    let (shapes, diags) = load_scenarios(&builtin_doc(&text), &schema);
    assert!(diags.is_empty(), "{diags:?}");
    assert!(
        shapes[0].sums_rows(),
        "Σ over tenors per shift is a bucket sum"
    );
    assert!(!shapes[0].sums_columns(), "no Σ across spot shifts");
    assert_eq!(shapes[0].totals_grouping(), Some(vec!["spot_shift".into()]));
}

#[test]
fn schema_refusals_name_their_field() {
    for (from, to, at) in [
        (
            "dataset = \"vega_buckets\"",
            "dataset = \"nonesuch\"",
            "scenarios.vega_kt.dataset",
        ),
        (
            "dataset = \"vega_buckets\"",
            "dataset = \"risk_snapshot\"",
            "scenarios.vega_kt.dataset",
        ),
        (
            "rows = \"tenor\"",
            "rows = \"book\"",
            "scenarios.vega_kt.rows",
        ),
        (
            r#"{ axis = "moneyness" }"#,
            r#"{ axis = "book" }"#,
            "scenarios.vega_kt.columns.axis.0",
        ),
        (
            "value = \"vega\"",
            "value = \"book\"",
            "scenarios.vega_kt.value",
        ),
        (
            "value = \"vega\"",
            "value = \"nonesuch\"",
            "scenarios.vega_kt.value",
        ),
    ] {
        assert_eq!(
            refused_by_check(&with(VEGA_KT, from, to)),
            at,
            "{from:?} → {to:?}"
        );
    }
    // An axis laid out nowhere would be summed across silently.
    assert_eq!(
        refused_by_check(&with(
            RHO,
            r#"["curve_ccy", "curve_type"]"#,
            r#"["curve_ccy"]"#
        )),
        "scenarios.rho.columns"
    );
    // A ladder over a two-axis dataset leaves one axis unlaid.
    let ladder_over_vega = with(
        &with(
            SPOT_LADDER,
            "dataset = \"spot_ladder\"",
            "dataset = \"vega_buckets\"",
        ),
        "rows = \"spot_shift\"",
        "rows = \"tenor\"",
    );
    let ladder_over_vega = with(
        &ladder_over_vega,
        "diff = { base = 0, measures = [\"npv\"] }\n",
        "",
    );
    let ladder_over_vega = ladder_over_vega
        .replace(
            "{ column = \"npv\", label = \"P&L vs base\" },",
            "{ column = \"vega\" },",
        )
        .replace(
            "{ column = \"delta\", label = \"Δ01\", format = { precision = 0, scale = \"k\" } },",
            "",
        )
        .replace("{ column = \"theta\", unit = \"per day\" },", "");
    assert_eq!(
        refused_by_check(&ladder_over_vega),
        "scenarios.spot_ladder.columns"
    );
}

#[test]
fn a_ladder_measure_the_dataset_lacks_is_refused_at_its_index() {
    assert_eq!(
        refused_by_check(&with(
            SPOT_LADDER,
            "{ column = \"theta\", unit = \"per day\" }",
            "{ column = \"gamma\" }"
        )),
        "scenarios.spot_ladder.columns.measures.2.column"
    );
    assert_eq!(
        refused_by_check(&with(
            SPOT_LADDER,
            "{ column = \"theta\", unit = \"per day\" }",
            "{ column = \"position_ref\" }"
        )),
        "scenarios.spot_ladder.columns.measures.2.column"
    );
}

#[test]
fn a_diff_base_must_match_the_rows_axis_type() {
    assert_eq!(
        refused_by_check(&with(SPOT_LADDER, "base = 0", r#"base = "0""#)),
        "scenarios.spot_ladder.diff.base"
    );
    // A text axis takes text, not a number.
    let text_rows = with(
        &with(
            RHO,
            r#"columns = { axis = ["curve_ccy", "curve_type"] }
value = "rho"
unit = "per 10 bp""#,
            r#"columns = { measures = [{ column = "rho" }] }
diff = { base = 1, measures = ["rho"] }"#,
        ),
        "rows = \"tenor\"",
        "rows = \"tenor\"",
    );
    let datasets_one_axis = DATASETS.replace(
        "axes = [\"tenor\", \"curve_ccy\", \"curve_type\"]",
        "axes = [\"tenor\"]",
    )
    .replace("[rho_curves.columns.curve_ccy]\ntype = \"utf8\"\nrole = \"axis\"\naxis = \"bucket\"\n", "")
    .replace("[rho_curves.columns.curve_type]\ntype = \"utf8\"\nrole = \"axis\"\naxis = \"bucket\"\n", "");
    let doc = merge_docs(
        "datasets",
        &[LayerDoc::builtin("datasets", &datasets_one_axis).unwrap()],
    );
    let (schema, diags) = SchemaSpec::from_doc(&doc);
    assert!(diags.is_empty(), "{diags:?}");
    let (shapes, diags) = load_scenarios(&builtin_doc(&text_rows), &schema);
    assert!(shapes.is_empty(), "{shapes:?}");
    assert_eq!(diags[0].path.as_deref(), Some("scenarios.rho.diff.base"));
}

#[test]
fn a_structural_refusal_is_reported_once_and_the_rest_still_check() {
    let text = format!(
        "{}\n{SPOT_LADDER}",
        with(VEGA_KT, "paint = \"heat\"", "paint = \"x\"")
    );
    let (shapes, diags) = load(&text);
    assert_eq!(shapes.len(), 1);
    assert_eq!(shapes[0].kind, "spot_ladder");
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].path.as_deref(), Some("scenarios.vega_kt.paint"));
}
