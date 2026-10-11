//! The builtin shapes as configuration. `geode-app` layers this under desk
//! and user `scenarios.toml` and runs `load_scenarios` over the merged
//! document against the assembled schema.

#[cfg(any(test, feature = "test-support"))]
use geode_core::config::{LayerDoc, merge_docs};
#[cfg(any(test, feature = "test-support"))]
use geode_core::scenario::{SCENARIOS_DOC, ShapeSpec, read_scenarios};
#[cfg(any(test, feature = "test-support"))]
use std::sync::{Arc, LazyLock};

/// The five shapes, in shipped order.
pub const BUILTIN_SCENARIOS: &str = include_str!("builtin_scenarios.toml");

/// The kinds `BUILTIN_SCENARIOS` defines, in document order.
pub const BUILTIN_KINDS: [&str; 5] = ["vega_kt", "vega_spot", "spot_ladder", "by_maturity", "rho"];

/// One builtin shape as its TOML reads, structurally — without the axes and
/// twins only the schema supplies. For tests and benches only: the
/// application loads shapes from its layered config through
/// `geode_core::scenario::load_scenarios`. Panics on a name the builtin
/// document does not define, a programmer error.
#[cfg(any(test, feature = "test-support"))]
pub fn builtin_shape(name: &str) -> Arc<ShapeSpec> {
    static SHAPES: LazyLock<Vec<Arc<ShapeSpec>>> = LazyLock::new(|| {
        let doc = LayerDoc::builtin(SCENARIOS_DOC, BUILTIN_SCENARIOS)
            .expect("BUILTIN_SCENARIOS is well-formed TOML");
        let (shapes, diags) = read_scenarios(&merge_docs(SCENARIOS_DOC, &[doc]));
        assert!(diags.is_empty(), "the builtin shapes read clean: {diags:?}");
        shapes.into_iter().map(Arc::new).collect()
    });
    SHAPES
        .iter()
        .find(|s| s.kind == name)
        .cloned()
        .unwrap_or_else(|| panic!("no builtin shape '{name}'"))
}

/// One builtin shape as `load_scenarios` hands it over against the demo
/// datasets: axes and twins filled. For the window tests and benches; the
/// application checks shapes against its own assembled schema.
#[cfg(any(test, feature = "test-support"))]
pub fn builtin_shape_checked(name: &str) -> Arc<ShapeSpec> {
    static SHAPES: LazyLock<Vec<Arc<ShapeSpec>>> = LazyLock::new(|| {
        let doc = LayerDoc::builtin(SCENARIOS_DOC, BUILTIN_SCENARIOS)
            .expect("BUILTIN_SCENARIOS is well-formed TOML");
        let (shapes, diags) = geode_core::scenario::load_scenarios(
            &merge_docs(SCENARIOS_DOC, &[doc]),
            &demo::demo_schema(),
        );
        assert!(
            diags.is_empty(),
            "the builtin shapes check clean: {diags:?}"
        );
        shapes.into_iter().map(Arc::new).collect()
    });
    SHAPES
        .iter()
        .find(|s| s.kind == name)
        .cloned()
        .unwrap_or_else(|| panic!("no builtin shape '{name}'"))
}

/// The demo schema the five builtin shapes are written against.
#[cfg(any(test, feature = "test-support"))]
pub mod demo {
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::schema::SchemaSpec;

    /// The demo's dataset declarations, the ones the five shapes are written
    /// against. Read from the example file so a renamed demo column refuses
    /// a builtin here, not at a trader's desk.
    pub const DEMO_DATASETS: &str = include_str!("../../../../examples/demo-config/datasets.toml");

    /// The demo datasets as a schema. Panics if they do not read clean, a
    /// defect in the example config.
    pub fn demo_schema() -> SchemaSpec {
        let doc = merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", DEMO_DATASETS).expect("demo datasets parse")],
        );
        let (schema, diags) = SchemaSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        schema
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::scenario::{Columns, DiffBase, Paint, load_scenarios, read_scenarios};
    use geode_core::schema::axis::AxisKind;

    #[test]
    fn the_builtin_shapes_read_clean_in_shipped_order() {
        let (shapes, diags) = read_scenarios(&merge_docs(
            SCENARIOS_DOC,
            &[LayerDoc::builtin(SCENARIOS_DOC, BUILTIN_SCENARIOS).unwrap()],
        ));
        assert!(diags.is_empty(), "{diags:?}");
        let kinds: Vec<&str> = shapes.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(kinds, BUILTIN_KINDS);
    }

    #[test]
    fn the_builtin_shapes_check_clean_against_the_demo_datasets() {
        let (shapes, diags) = load_scenarios(
            &merge_docs(
                SCENARIOS_DOC,
                &[LayerDoc::builtin(SCENARIOS_DOC, BUILTIN_SCENARIOS).unwrap()],
            ),
            &demo::demo_schema(),
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(shapes.len(), BUILTIN_KINDS.len());
        let by_kind = |k: &str| shapes.iter().find(|s| s.kind == k).unwrap();

        let vega_kt = by_kind("vega_kt");
        assert_eq!(
            vega_kt.value.as_ref().unwrap().usd_twin.as_deref(),
            Some("vega_usd")
        );
        assert!(vega_kt.sums_rows() && vega_kt.sums_columns());

        let vega_spot = by_kind("vega_spot");
        assert!(vega_spot.sums_rows(), "Σ over tenors per shift");
        assert!(!vega_spot.sums_columns(), "never across spot shifts");
        assert_eq!(
            vega_spot.axis("spot_shift").unwrap().kind,
            AxisKind::Scenario
        );

        let ladder = by_kind("spot_ladder");
        assert!(!ladder.sums_rows() && !ladder.sums_columns());
        let Columns::Measures(cols) = &ladder.columns else {
            unreachable!()
        };
        assert!(
            cols.iter().all(|c| c.usd_twin.is_some()),
            "every ladder greek has a USD twin"
        );
        assert_eq!(ladder.diff.as_ref().unwrap().base, DiffBase::Number(0.0));

        let by_maturity = by_kind("by_maturity");
        assert!(by_maturity.sums_rows(), "tenor buckets sum");
        assert_eq!(by_maturity.paint, Paint::Plain);
        assert_eq!(
            by_maturity.totals_grouping(),
            None,
            "a ladder's Σ row is the total row"
        );

        let rho = by_kind("rho");
        assert_eq!(rho.column_axes(), ["curve_ccy", "curve_type"]);
        assert_eq!(
            rho.totals_grouping(),
            Some(vec!["curve_ccy".into(), "curve_type".into()])
        );
        assert_eq!(
            rho.value.as_ref().unwrap().usd_twin.as_deref(),
            Some("rho_usd")
        );
    }

    #[test]
    fn builtin_shape_finds_each_kind_and_panics_on_a_stranger() {
        for kind in BUILTIN_KINDS {
            assert_eq!(builtin_shape(kind).kind, *kind);
        }
        assert!(std::panic::catch_unwind(|| builtin_shape("nonesuch")).is_err());
    }

    #[test]
    fn builtin_shape_checked_hands_over_twins_the_structural_read_lacks() {
        for kind in BUILTIN_KINDS {
            assert_eq!(builtin_shape_checked(kind).kind, *kind);
        }
        let twin = |s: &ShapeSpec| s.value.as_ref().unwrap().usd_twin.clone();
        assert_eq!(twin(&builtin_shape("vega_kt")), None);
        assert_eq!(
            twin(&builtin_shape_checked("vega_kt")).as_deref(),
            Some("vega_usd")
        );
        assert!(std::panic::catch_unwind(|| builtin_shape_checked("nonesuch")).is_err());
    }

    /// Every builtin shape derives a view the demo schema accepts, and the
    /// five carry `currency` ungrouped (every demo scenario dataset has it).
    #[test]
    fn the_builtin_shapes_derive_views_the_demo_schema_accepts() {
        use geode_core::config::Severity;
        use geode_core::dimensions::DerivedDimensions;
        use geode_core::view::ViewColumn;
        let schema = demo::demo_schema();
        let (shapes, diags) = load_scenarios(
            &merge_docs(
                SCENARIOS_DOC,
                &[LayerDoc::builtin(SCENARIOS_DOC, BUILTIN_SCENARIOS).unwrap()],
            ),
            &schema,
        );
        assert!(diags.is_empty(), "{diags:?}");
        for shape in &shapes {
            let ds = schema.dataset(&shape.dataset).unwrap();
            let view = shape.derive_view(ds);
            assert_eq!(view.name, format!("scenario:{}", shape.kind));
            assert_eq!(view.grouping, shape.grouping(), "{}", shape.kind);
            let errors: Vec<_> = view
                .validate(&schema, &DerivedDimensions::default())
                .into_iter()
                .filter(|d| d.severity == Severity::Error)
                .collect();
            assert!(errors.is_empty(), "{}: {errors:?}", shape.kind);
            assert!(
                view.columns
                    .iter()
                    .any(|c| matches!(c, ViewColumn::Dimension { name, .. } if name == "currency")),
                "{} carries currency",
                shape.kind
            );
            let measures: Vec<&str> = view
                .columns
                .iter()
                .filter(|c| matches!(c, ViewColumn::Measure { .. }))
                .map(ViewColumn::name)
                .collect();
            assert_eq!(measures, shape.query_measures(), "{}", shape.kind);
        }
    }
}
