//! The `pricer` dataset: the vocabulary the sheet paints, declared in the
//! shared schema so views, scopes, groupings and the Views dialog see it.
//! Computed — no source, no table, no query; the tile answers for it.
//! The column list mirrors `columns::COLUMNS` exactly (one test pins it).

pub const PRICER_DATASET: &str = "pricer";

/// Pushed into the builtin `datasets` layer by `geode-app` beside
/// `pricer_sheets`, and pinned the same way: a differing desk or user
/// redeclaration is replaced with an error diagnostic.
///
/// `underlying_ref` declares no grain: it is a key column of the
/// underlying grain, which the bare-dimension rule accepts, exactly as
/// `risk_snapshot` declares it. `sheet` and `template` are dimensions of
/// the position grain, whose keys every finer grain's key contains, so
/// the underlying grain the measures declare carries them. Measure and
/// identity spellings are `risk_snapshot`'s, so a scope or grouping
/// written against the blotter reads the same on a sheet.
pub const PRICER_DATASET_DECLARATION: &str = r#"[pricer]
computed = true

[pricer.columns.sheet]
type = "utf8"
role = "dimension"
grain = "position"
[pricer.columns.position_ref]
type = "utf8"
role = "key"
[pricer.columns.instrument_ref]
type = "utf8"
role = "key"
[pricer.columns.template]
type = "utf8"
role = "dimension"
grain = "position"
[pricer.columns.qty]
type = "i64"
role = "dimension"
grain = "instrument"
[pricer.columns.underlying_ref]
type = "utf8"
role = "dimension"
[pricer.columns.expiry]
type = "utf8"
role = "dimension"
grain = "instrument"
[pricer.columns.strike]
type = "f64"
role = "dimension"
grain = "instrument"
categorical = false
[pricer.columns.option_type]
type = "utf8"
role = "dimension"
grain = "instrument"
[pricer.columns.currency]
type = "utf8"
role = "dimension"
grain = "instrument"
[pricer.columns.barrier]
type = "f64"
role = "dimension"
grain = "instrument"
categorical = false
[pricer.columns.barrier_type]
type = "utf8"
role = "dimension"
grain = "instrument"
[pricer.columns.spot_shift]
type = "f64"
role = "dimension"
grain = "instrument"
categorical = false
[pricer.columns.vol_shift]
type = "f64"
role = "dimension"
grain = "instrument"
categorical = false

[pricer.columns.npv]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.npv_usd]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.delta01_usd]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.delta02]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.delta02_usd]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.delta05]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.delta05_usd]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.gamma01]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.gamma01_usd]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.gamma02]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.gamma02_usd]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.gamma05]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.gamma05_usd]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.vega01]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.vega01_usd]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.normalized_vega01]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.normalized_vega01_usd]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.skew01]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.skew01_usd]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.rho010]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.rho010_usd]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.rho_rfr010]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.rho_rfr010_usd]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.rho_ois010]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.rho_ois010_usd]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.clean_theta_business_day]
type = "f64"
role = "measure"
grain = "underlying"
[pricer.columns.clean_theta_business_day_usd]
type = "f64"
role = "measure"
grain = "underlying"

[pricer.columns.priced_at]
type = "utf8"
role = "dimension"
grain = "instrument"
categorical = false
[pricer.columns.status]
type = "utf8"
role = "dimension"
grain = "instrument"
categorical = false
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::columns::COLUMNS;
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::pricing::Measure;
    use geode_core::schema::{ColumnRole, SchemaSpec};

    fn schema() -> SchemaSpec {
        let (schema, diags) = SchemaSpec::from_doc(&merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", PRICER_DATASET_DECLARATION).unwrap()],
        ));
        assert!(diags.is_empty(), "{diags:?}");
        schema
    }

    #[test]
    fn the_declaration_parses_clean_and_is_computed() {
        let schema = schema();
        let ds = schema.dataset(PRICER_DATASET).expect("declared");
        assert!(ds.computed);
        // Schema order: every key and dimension some declared grain
        // carries; no measure.
        assert_eq!(
            ds.groupable_columns(),
            vec![
                "sheet",
                "position_ref",
                "instrument_ref",
                "template",
                "qty",
                "underlying_ref",
                "expiry",
                "strike",
                "option_type",
                "currency",
                "barrier",
                "barrier_type",
                "spot_shift",
                "vol_shift",
                "priced_at",
                "status"
            ]
        );
    }

    #[test]
    fn the_declaration_names_exactly_the_paint_vocabulary() {
        let schema = schema();
        let ds = schema.dataset(PRICER_DATASET).unwrap();
        let declared: Vec<&str> = ds.columns.iter().map(|c| c.name.as_str()).collect();
        let painted: Vec<&str> = COLUMNS.iter().map(|c| c.name).collect();
        assert_eq!(
            declared, painted,
            "one vocabulary: the dataset and COLUMNS list the same names in the same order"
        );
        for m in Measure::ALL {
            for name in [m.name(), m.usd_name()] {
                let c = ds.column(name).unwrap();
                assert!(
                    matches!(c.role, ColumnRole::Measure { .. }),
                    "{name} is a measure"
                );
            }
        }
    }
}
