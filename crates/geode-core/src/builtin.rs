//! The datasets every Geode build declares, shared by the app and the
//! background collector so both create and read the same tables. They live
//! here, below every gpui crate, because the gpui-free composition crate
//! (`geode-compose`) puts them in the builtin config layer; `geode-pricer`
//! re-exports them under its own paths.

pub const PRICER_SHEETS_DATASET: &str = "pricer_sheets";

/// The datasets-doc declaration, one `[pricer_sheets.columns.<name>]`
/// table per column. `geode_compose::builtin_data_layer` puts it in the
/// builtin config layer.
///
/// **Frozen.** The store creates the tables with `CREATE TABLE IF NOT
/// EXISTS` and publishes insert positionally, so once a database holds
/// `pricer_sheets` its column list and order cannot change without a
/// migration, which does not exist: a changed declaration would write
/// values into the wrong columns of an existing database. Add, remove or
/// reorder a column only together with a migration.
///
/// `currency` was appended on 2026-10-03; databases created before then
/// are refused by the drift check and must be cleared. It is the last
/// value column, so values keep their positions ahead of it. `""` is a
/// line without a payout currency, and every package.
///
/// `sheet` is `categorical = false`: a text dimension is categorical by
/// default, which would offer sheet names in the frame picker and the
/// groupings and rebuild an ENUM on every autosave. A sheet name is not a
/// scope dimension.
pub const PRICER_SHEETS_DECLARATION: &str = r#"[pricer_sheets]
family = "document"
local = true
key = ["sheet"]
axes = ["line"]

[pricer_sheets.columns.sheet]
type = "utf8"
role = "dimension"
textual = true
categorical = false
[pricer_sheets.columns.line]
type = "i64"
role = "axis"

[pricer_sheets.columns.order]
type = "i64"
role = "value"
[pricer_sheets.columns.kind]
type = "utf8"
role = "value"
[pricer_sheets.columns.template]
type = "utf8"
role = "value"
[pricer_sheets.columns.parent]
type = "i64"
role = "value"
[pricer_sheets.columns.qty]
type = "i64"
role = "value"
[pricer_sheets.columns.underlying]
type = "utf8"
role = "value"
[pricer_sheets.columns.expiry_kind]
type = "utf8"
role = "value"
[pricer_sheets.columns.expiry]
type = "utf8"
role = "value"
[pricer_sheets.columns.strike_kind]
type = "utf8"
role = "value"
[pricer_sheets.columns.strike]
type = "f64"
role = "value"
[pricer_sheets.columns.option_kind]
type = "utf8"
role = "value"
[pricer_sheets.columns.barrier_kind]
type = "utf8"
role = "value"
[pricer_sheets.columns.barrier]
type = "f64"
role = "value"
[pricer_sheets.columns.spot_shift_own]
type = "i64"
role = "value"
[pricer_sheets.columns.spot_shift]
type = "f64"
role = "value"
[pricer_sheets.columns.vol_shift_own]
type = "i64"
role = "value"
[pricer_sheets.columns.vol_shift]
type = "f64"
role = "value"
[pricer_sheets.columns.currency]
type = "utf8"
role = "value"

[pricer_sheets.columns.view]
type = "utf8"
role = "attribute"
[pricer_sheets.columns.sheet_spot_shift_own]
type = "i64"
role = "attribute"
[pricer_sheets.columns.sheet_spot_shift]
type = "f64"
role = "attribute"
[pricer_sheets.columns.sheet_vol_shift_own]
type = "i64"
role = "attribute"
[pricer_sheets.columns.sheet_vol_shift]
type = "f64"
role = "attribute"
[pricer_sheets.columns.refresh]
type = "utf8"
role = "attribute"
[pricer_sheets.columns.spot_overrides]
type = "utf8"
role = "attribute"
"#;

pub const PRICER_DATASET: &str = "pricer";

/// Put in the builtin `datasets` layer by
/// `geode_compose::builtin_data_layer` beside `pricer_sheets`, and pinned
/// the same way: a differing desk or user redeclaration is replaced with
/// an error diagnostic.
///
/// `underlying_ref` declares no grain: it is a key column of the
/// underlying grain, which the bare-dimension rule accepts, exactly as
/// `risk_snapshot` declares it. `sheet` and `template` are dimensions of
/// the position grain, whose keys every finer grain's key contains, so
/// the underlying grain the measures declare carries them. Measure and
/// identity spellings are `risk_snapshot`'s, so a scope or grouping
/// written against the blotter reads the same on a sheet. `currency` is
/// the line's payout currency, an input of the instrument grain (blank,
/// so NULL, on a line not yet given one), not a property of its result.
pub const PRICER_DATASET_DECLARATION: &str = r#"[pricer]
computed = true

[pricer.columns.sheet]
type = "utf8"
role = "dimension"
textual = true
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
textual = true
grain = "position"
[pricer.columns.qty]
type = "i64"
role = "dimension"
grain = "instrument"
[pricer.columns.underlying_ref]
type = "utf8"
role = "dimension"
textual = true
[pricer.columns.expiry]
type = "utf8"
role = "dimension"
textual = true
grain = "instrument"
[pricer.columns.strike]
type = "f64"
role = "dimension"
grain = "instrument"
categorical = false
[pricer.columns.option_type]
type = "utf8"
role = "dimension"
textual = true
grain = "instrument"
[pricer.columns.currency]
type = "utf8"
role = "dimension"
textual = true
grain = "instrument"
[pricer.columns.barrier]
type = "f64"
role = "dimension"
grain = "instrument"
categorical = false
[pricer.columns.barrier_type]
type = "utf8"
role = "dimension"
textual = true
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
textual = true
grain = "instrument"
categorical = false
[pricer.columns.status]
type = "utf8"
role = "dimension"
textual = true
grain = "instrument"
categorical = false
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};
    use crate::schema::SchemaSpec;

    /// Both declarations parse, declare exactly the dataset they are named
    /// for, and compare equal to themselves (the equality the
    /// equal-configuration contract relies on).
    #[test]
    fn each_declaration_declares_its_named_dataset() {
        for (name, text) in [
            (PRICER_SHEETS_DATASET, PRICER_SHEETS_DECLARATION),
            (PRICER_DATASET, PRICER_DATASET_DECLARATION),
        ] {
            let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
            let (schema, diags) = SchemaSpec::from_doc(&doc);
            assert!(diags.is_empty(), "{name}: {diags:?}");
            let names: Vec<&str> = schema.datasets.iter().map(|d| d.name.as_str()).collect();
            assert_eq!(names, [name]);
            assert_eq!(schema, schema.clone());
        }
    }
}
