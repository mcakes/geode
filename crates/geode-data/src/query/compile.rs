//! The view compiler (spec §6.3). One statement per view, covering every
//! level of the rollup tree, with each measure aggregated at its own grain
//! and joined at group cardinality.
//!
//! Emitting one query per level would put tree navigation on the 50ms
//! requery budget; emitting one puts expand and collapse on the 8ms frame
//! budget instead, because every level is already in the snapshot.

use crate::query::scope_sql::compile_scope;
use crate::store::StoreError;
use crate::store::ddl::{TableKind, table_name};
use duckdb::Connection;
use duckdb::types::Value;
use geode_core::attribution::{Attribution, ScopeSemantics, attribution_of};
use geode_core::dimensions::DerivedDimensions;
use geode_core::schema::{Aggregate, ColumnRole, ColumnSpec, Grain, SchemaSpec};
use geode_core::scope::Scope;
use geode_core::view::{ViewColumn, ViewSpec};

/// `GROUPING(a, b, c)` returns a bitmask: a bit is set for each column the
/// row is *not* grouped by. Under ROLLUP, a level with `depth` of `n`
/// columns present has the top `n - depth` bits set.
pub fn mask_for_depth(n: usize, depth: usize) -> i64 {
    ((1i64 << (n - depth)) - 1).max(0)
}

/// Inverse of [`mask_for_depth`], or `None` when the mask is not one
/// ROLLUP can produce.
pub fn depth_for_mask(n: usize, mask: i64) -> Option<usize> {
    (0..=n).find(|d| mask_for_depth(n, *d) == mask)
}

#[derive(Debug, Clone)]
pub struct CompiledColumn {
    pub name: String,
    /// `None` for grouping columns and the depth marker.
    pub grain: Option<Grain>,
    /// Indexed by depth, `0..=grouping.len()`.
    pub attribution_by_depth: Vec<Attribution>,
    pub scope_semantics: ScopeSemantics,
}

#[derive(Debug, Clone)]
pub struct CompiledQuery {
    pub sql: String,
    pub params: Vec<Value>,
    /// Temp tables the scope compiler created; the caller drops them.
    pub temp_tables: Vec<String>,
    pub grouping: Vec<String>,
    pub columns: Vec<CompiledColumn>,
}

fn quoted(cols: &[String]) -> Vec<String> {
    cols.iter().map(|c| format!("\"{c}\"")).collect()
}

/// The finest declared grain whose key carries every grouping column.
///
/// It must be *declared* — `apply_schema` only creates tables for grains
/// the dataset has measures or attributes at, so picking the finest grain
/// unconditionally would name a table that does not exist. It must carry
/// every grouping column, or it cannot produce the levels of the tree.
fn spine_grain_for(
    ds: &geode_core::schema::DatasetSpec,
    view: &ViewSpec,
    dims: &DerivedDimensions,
) -> Option<Grain> {
    ds.grains().into_iter().rev().find(|g| {
        view.grouping
            .iter()
            .all(|col| g.key_columns().contains(&dims.base_column(col)))
    })
}

/// Derived ENUM type names currently present for a dataset.
fn existing_enum_types(conn: &Connection, dataset: &str) -> Result<Vec<String>, StoreError> {
    let sql = "select type_name from duckdb_types() where type_name like ?";
    let err = |source| StoreError::Sql {
        statement: sql.to_string(),
        source,
    };
    let mut stmt = conn.prepare(sql).map_err(err)?;
    let rows = stmt
        .query_map(duckdb::params![format!("{dataset}_%_enum")], |r| {
            r.get::<_, String>(0)
        })
        .map_err(err)?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

pub fn compile_view(
    conn: &Connection,
    view: &ViewSpec,
    schema: &SchemaSpec,
    scope: &Scope,
    dims: &DerivedDimensions,
) -> Result<CompiledQuery, StoreError> {
    let ds = schema
        .dataset(&view.dataset)
        .ok_or_else(|| StoreError::Sql {
            statement: format!("compile view '{}'", view.name),
            source: duckdb::Error::InvalidParameterName(format!(
                "unknown dataset '{}'",
                view.dataset
            )),
        })?;

    let n = view.grouping.len();
    let group_cols = quoted(&view.grouping);
    let mut params: Vec<Value> = Vec::new();
    let mut temp_tables: Vec<String> = Vec::new();
    let mut ctes: Vec<String> = Vec::new();
    let mut selects: Vec<String> = Vec::new();
    let mut joins: Vec<String> = Vec::new();
    let mut columns: Vec<CompiledColumn> = Vec::new();

    // The spine must be able to produce every level, so it needs every
    // grouping column — and it must be a grain the dataset actually
    // declares, because only those have tables. The finest such grain is
    // the most selective spine.
    let spine_grain = spine_grain_for(ds, view, dims).ok_or_else(|| StoreError::Sql {
        statement: format!("compile view '{}'", view.name),
        source: duckdb::Error::InvalidParameterName(format!(
            "no declared grain of '{}' carries every grouping column {:?}; \
             a grouping column must be a key column of some grain",
            view.dataset, view.grouping
        )),
    })?;
    let spine_scope = compile_scope(conn, scope, ds, spine_grain, dims, spine_grain)?;
    params.extend(spine_scope.params.clone());
    temp_tables.extend(spine_scope.temp_tables.clone());

    if n == 0 {
        // No grouping columns: the tree is a single grand-total row. The
        // spine selects it without a table at all — reading the table
        // without a GROUP BY would emit one spine row per *input* row, and
        // the scope is applied inside each measure subquery anyway.
        ctes.push("spine as (select 0 as depth_mask)".to_string());
        // The spine's own scope params are unused in this shape.
        params.clear();
        temp_tables.extend(spine_scope.temp_tables.clone());
    } else {
        ctes.push(format!(
            "spine as (select {select}, grouping({group}) as depth_mask \
             from {table} where {pred} group by rollup({group}))",
            select = group_cols.join(", "),
            group = group_cols.join(", "),
            table = table_name(&view.dataset, spine_grain, TableKind::Live),
            pred = spine_scope.predicate,
        ));
    }

    // Cast only to types that actually exist: the derived ENUMs are built
    // by ingest, so before the first load there are none and the cast
    // would be a hard error. Degrading to plain strings is correct — the
    // interning is an optimization, not a semantic.
    let interned: Vec<&str> = {
        let existing = existing_enum_types(conn, &view.dataset)?;
        crate::store::ddl::dimension_columns(ds)
            .into_iter()
            .filter(|c| existing.contains(&crate::store::ddl::enum_type_name(&view.dataset, c)))
            .collect()
    };
    for g in &view.grouping {
        // Dimension columns are cast to their derived ENUM so the result
        // comes back dictionary-encoded rather than as strings (spec
        // §6.6, §7.2) — the renderer then compares on integer codes.
        if interned.contains(&g.as_str()) {
            selects.push(format!(
                "s.\"{g}\"::{} as \"{g}\"",
                crate::store::ddl::enum_type_name(&view.dataset, g)
            ));
        } else {
            selects.push(format!("s.\"{g}\""));
        }
        columns.push(CompiledColumn {
            name: g.clone(),
            grain: None,
            attribution_by_depth: vec![Attribution::Additive; n + 1],
            scope_semantics: ScopeSemantics::Direct,
        });
    }
    selects.push("s.depth_mask".to_string());
    columns.push(CompiledColumn {
        name: "depth_mask".to_string(),
        grain: None,
        attribution_by_depth: vec![Attribution::Additive; n + 1],
        scope_semantics: ScopeSemantics::Direct,
    });

    // One aggregate subquery per measure grain the view touches.
    for grain in view.measure_grains(schema) {
        let alias = format!("agg_{}", grain.table());
        let grain_scope = compile_scope(conn, scope, ds, grain, dims, spine_grain)?;
        temp_tables.extend(grain_scope.temp_tables.clone());

        // Only the grouping columns this grain actually has.
        let own: Vec<String> = view
            .grouping
            .iter()
            .filter(|g| grain.key_columns().contains(&dims.base_column(g)))
            .cloned()
            .collect();
        let own_q = quoted(&own);

        let measures: Vec<&ColumnSpec> = view
            .columns
            .iter()
            .filter_map(|c| match c {
                ViewColumn::Measure { name } => ds.column(name),
                _ => None,
            })
            .filter(|c| c.grain() == Some(grain))
            .collect();
        if measures.is_empty() {
            continue;
        }

        let aggs: Vec<String> = measures
            .iter()
            .map(|m| {
                let agg = match m.role {
                    ColumnRole::Measure { aggregate, .. } => aggregate,
                    _ => Aggregate::Sum,
                };
                format!("{} as \"{}\"", agg.sql(&format!("\"{}\"", m.name)), m.name)
            })
            .collect();

        let sub_group = if own.is_empty() {
            String::new()
        } else {
            format!(" group by rollup({})", own_q.join(", "))
        };
        ctes.push(format!(
            "{alias} as (select {keys}{comma}{aggs} from {table} where {pred}{sub_group})",
            keys = own_q.join(", "),
            comma = if own.is_empty() { "" } else { ", " },
            aggs = aggs.join(", "),
            table = table_name(&view.dataset, grain, TableKind::Live),
            pred = grain_scope.predicate,
        ));
        // The grain subquery's params follow the spine's, in CTE order.
        params.extend(grain_scope.params);

        let on = if own.is_empty() {
            "true".to_string()
        } else {
            own.iter()
                .map(|c| format!("{alias}.\"{c}\" is not distinct from s.\"{c}\""))
                .collect::<Vec<_>>()
                .join(" and ")
        };
        joins.push(format!("left join {alias} on {on}"));

        for m in measures {
            // Attribution per depth, from the schema alone.
            let by_depth: Vec<Attribution> = (0..=n)
                .map(|d| attribution_of(grain, &view.grouping[..d], dims))
                .collect();
            let blank: Vec<String> = (0..=n)
                .filter(|d| by_depth[*d] == Attribution::NonAttributable)
                .map(|d| mask_for_depth(n, d).to_string())
                .collect();

            let expr = if blank.is_empty() {
                format!("{alias}.\"{}\"", m.name)
            } else {
                // The value would belong to an ancestor row, not this one.
                format!(
                    "case when s.depth_mask in ({}) then null else {alias}.\"{}\" end",
                    blank.join(", "),
                    m.name
                )
            };
            selects.push(format!("{expr} as \"{}\"", m.name));
            columns.push(CompiledColumn {
                name: m.name.clone(),
                grain: Some(grain),
                attribution_by_depth: by_depth,
                scope_semantics: grain_scope.semantics.clone(),
            });
        }
    }

    // Derived columns are expressions over the columns already selected.
    for c in &view.columns {
        if let ViewColumn::Derived { name, sql } = c {
            selects.push(format!("({sql}) as \"{name}\""));
            columns.push(CompiledColumn {
                name: name.clone(),
                grain: None,
                attribution_by_depth: vec![Attribution::Additive; n + 1],
                scope_semantics: ScopeSemantics::Direct,
            });
        }
    }

    let order = if view.sort.is_empty() {
        String::new()
    } else {
        let keys: Vec<String> = view
            .sort
            .iter()
            .map(|s| {
                format!(
                    "\"{}\" {}",
                    s.column,
                    if s.descending { "desc" } else { "asc" }
                )
            })
            .collect();
        format!(" order by s.depth_mask desc, {}", keys.join(", "))
    };

    let sql = format!(
        "with {ctes} select {selects} from spine s {joins}{order}",
        ctes = ctes.join(",\n"),
        selects = selects.join(", "),
        joins = joins.join(" "),
    );

    Ok(CompiledQuery {
        sql,
        params,
        temp_tables,
        grouping: view.grouping.clone(),
        columns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::attribution::Attribution;
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::dimensions::DerivedDimensions;
    use geode_core::schema::SchemaSpec;
    use geode_core::scope::Scope;
    use geode_core::view::ViewSpec;

    fn schema() -> SchemaSpec {
        let text = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.lhu]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
[risk_snapshot.columns.counterparty]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.instrument_ref]
type = "utf8"
role = "key"
[risk_snapshot.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.underlying2_ref]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
[risk_snapshot.columns.daily_trading_pnl]
type = "f64"
role = "measure"
grain = "position"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc).0
    }

    /// The spec §6.3 view: lhu > underlying > position, one measure at
    /// underlying grain and one at position grain.
    fn view() -> ViewSpec {
        let text = r#"
[tree]
dataset = "risk_snapshot"
grouping = ["lhu", "underlying_ref", "position_ref"]
[[tree.columns]]
name = "delta01"
kind = "measure"
[[tree.columns]]
name = "daily_trading_pnl"
kind = "measure"
"#;
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        ViewSpec::from_doc(&doc).0.into_iter().next().unwrap()
    }

    /// A store with the live tables and one worst-of instrument: position
    /// P1 in LHU L0, two underlyings, trading PnL 7, delta 10 and 20.
    fn fixture() -> (tempfile::TempDir, crate::store::Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        store
            .apply_schema(schema().dataset("risk_snapshot").unwrap())
            .unwrap();
        store
            .writer()
            .execute_batch(
                "insert into risk_snapshot_position_live values
                   ('BK0','L0','P1','C', 7, 'b', 1, 1, now());
                 insert into risk_snapshot_underlying_live values
                   ('BK0','L0','P1','C','I1','SPX', 10, 'b', 1, 1, now()),
                   ('BK0','L0','P1','C','I1','RUT', 20, 'b', 1, 1, now());",
            )
            .unwrap();
        (dir, store)
    }

    fn compile(store: &crate::store::Store) -> CompiledQuery {
        compile_view(
            store.writer(),
            &view(),
            &schema(),
            &Scope::default(),
            &DerivedDimensions::default(),
        )
        .unwrap()
    }

    #[test]
    fn depth_and_mask_convert_both_ways() {
        // n = 3: leaf is 0, then 1, 3, and the grand total 7.
        assert_eq!(mask_for_depth(3, 3), 0);
        assert_eq!(mask_for_depth(3, 2), 1);
        assert_eq!(mask_for_depth(3, 1), 3);
        assert_eq!(mask_for_depth(3, 0), 7);
        for n in 0..=4 {
            for d in 0..=n {
                assert_eq!(depth_for_mask(n, mask_for_depth(n, d)), Some(d));
            }
        }
    }

    #[test]
    fn attribution_is_recorded_per_depth_matching_the_spec_example() {
        let (_d, store) = fixture();
        let q = compile(&store);
        let pnl = q
            .columns
            .iter()
            .find(|c| c.name == "daily_trading_pnl")
            .expect("pnl column");
        // depth 1 = [lhu], 2 = [lhu, underlying], 3 = [.., position]
        assert_eq!(pnl.attribution_by_depth[1], Attribution::Additive);
        assert_eq!(pnl.attribution_by_depth[2], Attribution::NonAttributable);
        assert_eq!(
            pnl.attribution_by_depth[3],
            Attribution::DeterminedNonAdditive
        );

        let delta = q.columns.iter().find(|c| c.name == "delta01").unwrap();
        for d in 0..=3 {
            assert_eq!(
                delta.attribution_by_depth[d],
                Attribution::Additive,
                "depth {d}"
            );
        }
    }

    #[test]
    fn one_statement_returns_every_level() {
        let (_d, store) = fixture();
        let q = compile(&store);
        let conn = store.writer();
        let mut stmt = conn.prepare(&q.sql).unwrap();
        let rows: Vec<(i64, Option<f64>, Option<f64>)> = stmt
            .query_map(duckdb::params_from_iter(q.params.iter()), |r| {
                Ok((
                    r.get("depth_mask")?,
                    r.get("delta01")?,
                    r.get("daily_trading_pnl")?,
                ))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        // Grand total, lhu, lhu+underlying (x2), lhu+underlying+position (x2)
        assert_eq!(rows.len(), 6, "{rows:?}");
        assert!(rows.iter().any(|(m, ..)| *m == 7), "grand total missing");
    }

    #[test]
    fn a_coarse_measure_does_not_double_count_at_any_level() {
        // The whole point. P1's trading PnL is 7; it must never sum to 14
        // just because the position has two underlyings.
        let (_d, store) = fixture();
        let q = compile(&store);
        let conn = store.writer();
        let mut stmt = conn.prepare(&q.sql).unwrap();
        let rows: Vec<(i64, Option<f64>)> = stmt
            .query_map(duckdb::params_from_iter(q.params.iter()), |r| {
                Ok((r.get("depth_mask")?, r.get("daily_trading_pnl")?))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        let at = |depth: usize| -> Vec<Option<f64>> {
            let m = mask_for_depth(3, depth);
            rows.iter()
                .filter(|(mask, _)| *mask == m)
                .map(|(_, v)| *v)
                .collect()
        };
        assert_eq!(
            at(0),
            vec![Some(7.0)],
            "grand total is the position's own PnL"
        );
        assert_eq!(at(1), vec![Some(7.0)], "LHU total, not doubled");
        assert_eq!(at(2), vec![None, None], "blank under an underlying");
        assert_eq!(
            at(3),
            vec![Some(7.0), Some(7.0)],
            "the position's real PnL, repeated and marked do-not-total"
        );
    }

    #[test]
    fn an_additive_measure_totals_correctly_up_the_tree() {
        let (_d, store) = fixture();
        let q = compile(&store);
        let conn = store.writer();
        let mut stmt = conn.prepare(&q.sql).unwrap();
        let rows: Vec<(i64, Option<f64>)> = stmt
            .query_map(duckdb::params_from_iter(q.params.iter()), |r| {
                Ok((r.get("depth_mask")?, r.get("delta01")?))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let total: Option<f64> = rows
            .iter()
            .find(|(m, _)| *m == mask_for_depth(3, 0))
            .map(|(_, v)| *v)
            .unwrap();
        assert_eq!(total, Some(30.0), "10 + 20 across the two underlyings");
    }

    #[test]
    fn the_grouping_columns_are_reported_in_order() {
        let (_d, store) = fixture();
        let q = compile(&store);
        assert_eq!(q.grouping, vec!["lhu", "underlying_ref", "position_ref"]);
    }

    #[test]
    fn an_empty_grouping_yields_a_single_total_row() {
        let (_d, store) = fixture();
        let mut v = view();
        v.grouping.clear();
        let q = compile_view(
            store.writer(),
            &v,
            &schema(),
            &Scope::default(),
            &DerivedDimensions::default(),
        )
        .unwrap();
        let conn = store.writer();
        let mut stmt = conn.prepare(&q.sql).unwrap();
        let n = stmt
            .query_map(duckdb::params_from_iter(q.params.iter()), |_| Ok(()))
            .unwrap()
            .count();
        assert_eq!(n, 1);
    }
}
