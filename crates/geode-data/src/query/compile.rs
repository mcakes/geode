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
    pub grouping: Vec<String>,
    pub columns: Vec<CompiledColumn>,
    /// Every dataset the query reads. A joined view is as stale as its
    /// stalest input (spec §5.4), and the caller cannot compute that
    /// without knowing which datasets were touched.
    pub stalest_input: Vec<String>,
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

/// `GROUPING SETS` covering depths `0..=max_depth` — the prefixes of the
/// grouping tuple, and nothing deeper.
///
/// A full `ROLLUP` materializes every level including the leaves, so a
/// collapsed tree still pays for rows nobody is looking at: at 1M rows
/// that is 398k rows produced to display 80 (`docs/perf.md`). It also
/// makes the blotter's flatten walk — grouping snapshot rows by parent to
/// build the visible row list gpui-component's `TableDelegate` indexes
/// into — proportional to what was materialized rather than to what is on
/// screen.
///
/// The caller materializes one level deeper than what is open, so a
/// single-step expand is already in hand and only a deeper one requeries.
fn grouping_sets(group_cols: &[String], max_depth: usize) -> String {
    let depth = max_depth.min(group_cols.len());
    let sets: Vec<String> = (0..=depth)
        .map(|d| format!("({})", group_cols[..d].join(", ")))
        .collect();
    format!("grouping sets ({})", sets.join(", "))
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
    as_of: &crate::query::as_of::AsOf,
    // Deepest grouping level to materialize. The caller passes one more
    // than what is expanded, so a single-step expand needs no requery.
    max_depth: usize,
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
    let depth = max_depth.min(n);
    let group_cols = quoted(&view.grouping);
    let mut params: Vec<Value> = Vec::new();
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
    // Same statement shape, different tables (spec §6.5). Only the as-of
    // path pays for history; live carries no generation predicate at all.
    let (kind_for_joins, gen_pred) = match as_of {
        crate::query::as_of::AsOf::Live => (TableKind::Live, None),
        crate::query::as_of::AsOf::At(t) => {
            let archive = table_name(&view.dataset, spine_grain, TableKind::Archive);
            let gens = crate::query::as_of::resolve_generations(conn, &archive, *t)?;
            (
                TableKind::Archive,
                Some(crate::query::as_of::generation_predicate(&gens)),
            )
        }
    };
    let and_gen = |p: &str| match &gen_pred {
        Some(g) => format!("({p}) and ({g})"),
        None => p.to_string(),
    };
    let spine_scope = compile_scope(conn, scope, ds, spine_grain, dims, spine_grain)?;
    params.extend(spine_scope.params.clone());

    if n == 0 {
        // No grouping columns: the tree is a single grand-total row. The
        // spine selects it without a table at all — reading the table
        // without a GROUP BY would emit one spine row per *input* row, and
        // the scope is applied inside each measure subquery anyway.
        ctes.push("spine as (select 0 as row_depth)".to_string());
        // The spine's own scope params are unused in this shape.
        params.clear();
    } else if depth == 0 {
        // Bounded to the grand total: one row, depth zero. The aggregate
        // is what makes it exactly one row.
        ctes.push(format!(
            "spine as (select 0 as row_depth, count(*) as _rows \
             from {table} where {pred})",
            table = table_name(&view.dataset, spine_grain, kind_for_joins),
            pred = and_gen(&spine_scope.predicate),
        ));
    } else {
        // `grouping()` may only name columns some set groups by, and its
        // width would then change with the bound — so the spine emits an
        // explicit depth instead of a bitmask. Under prefix sets a level
        // with `p` of `depth` columns present sets the top `depth - p`
        // bits, so popcount recovers the depth directly.
        let materialized = &group_cols[..depth];
        ctes.push(format!(
            "spine as (select {select}, \
             ({depth} - bit_count(grouping({group}))) as row_depth \
             from {table} where {pred} group by {sets})",
            select = materialized.join(", "),
            group = materialized.join(", "),
            table = table_name(&view.dataset, spine_grain, kind_for_joins),
            pred = and_gen(&spine_scope.predicate),
            sets = grouping_sets(&group_cols, depth),
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
    for (i, g) in view.grouping.iter().enumerate() {
        // Dimension columns are cast to their derived ENUM so the result
        // comes back dictionary-encoded rather than as strings (spec
        // §6.6, §7.2) — the renderer then compares on integer codes.
        //
        // Below the materialized bound the column is not in the spine at
        // all. It is still selected, as the NULL a rolled-up level would
        // carry, so the snapshot's shape does not depend on the bound.
        let ty = if interned.contains(&g.as_str()) {
            crate::store::ddl::enum_type_name(&view.dataset, g)
        } else {
            "varchar".to_string()
        };
        if i >= depth {
            selects.push(format!("NULL::{ty} as \"{g}\""));
        } else {
            selects.push(format!("s.\"{g}\"::{ty} as \"{g}\""));
        }
        columns.push(CompiledColumn {
            name: g.clone(),
            grain: None,
            attribution_by_depth: vec![Attribution::Additive; n + 1],
            scope_semantics: ScopeSemantics::Direct,
        });
    }
    selects.push("s.row_depth".to_string());
    columns.push(CompiledColumn {
        name: "row_depth".to_string(),
        grain: None,
        attribution_by_depth: vec![Attribution::Additive; n + 1],
        scope_semantics: ScopeSemantics::Direct,
    });

    // One aggregate subquery per measure grain the view touches.
    for grain in view.measure_grains(schema) {
        let alias = format!("agg_{}", grain.table());
        let grain_scope = compile_scope(conn, scope, ds, grain, dims, spine_grain)?;

        // Only the grouping columns this grain has, and only within the
        // materialized depth — selecting a key the spine no longer groups
        // by would leave it outside every aggregate.
        let own: Vec<String> = view.grouping[..depth]
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

        // The same depths, projected onto the columns this grain has.
        let sub_group = if own.is_empty() {
            String::new()
        } else {
            let mut sets: Vec<String> = (0..=depth)
                .map(|d| {
                    let kept: Vec<String> = own
                        .iter()
                        .filter(|c| view.grouping[..d].contains(c))
                        .map(|c| format!("\"{c}\""))
                        .collect();
                    format!("({})", kept.join(", "))
                })
                .collect();
            sets.dedup();
            format!(" group by grouping sets ({})", sets.join(", "))
        };
        ctes.push(format!(
            "{alias} as (select {keys}{comma}{aggs} from {table} where {pred}{sub_group})",
            keys = own_q.join(", "),
            comma = if own.is_empty() { "" } else { ", " },
            aggs = aggs.join(", "),
            table = table_name(&view.dataset, grain, kind_for_joins),
            pred = and_gen(&grain_scope.predicate),
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
                .map(|d| d.to_string())
                .collect();

            let expr = if blank.is_empty() {
                format!("{alias}.\"{}\"", m.name)
            } else {
                // The value would belong to an ancestor row, not this one.
                format!(
                    "case when s.row_depth in ({}) then null else {alias}.\"{}\" end",
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

    // Cross-dataset joins (spec §6.4). Join keys are declared in schema
    // config and joined onto the spine.
    //
    // A join is only meaningful when the grouping reaches the joined
    // dataset's key: above that level several instruments share the row,
    // and ROLLUP has already NULLed the key, so the join naturally yields
    // NULL — which is the honest answer rather than an arbitrary pick.
    let mut stalest_input = vec![view.dataset.clone()];
    for (i, join) in view.joins.iter().enumerate() {
        let Some(joined_ds) = schema.dataset(&join.dataset) else {
            continue;
        };
        stalest_input.push(join.dataset.clone());

        if !join.on.iter().all(|k| view.grouping.contains(k)) {
            // Nothing to join against: the key is not on the spine.
            continue;
        }
        let Some(joined_grain) = joined_ds.grains().into_iter().find(|g| {
            join.on
                .iter()
                .all(|k| g.key_columns().contains(&k.as_str()))
        }) else {
            continue;
        };

        let alias = format!("join_{i}");
        let on = join
            .on
            .iter()
            .map(|k| format!("{alias}.\"{k}\" is not distinct from s.\"{k}\""))
            .collect::<Vec<_>>()
            .join(" and ");
        joins.push(format!(
            "left join {} {alias} on {on}",
            table_name(&join.dataset, joined_grain, kind_for_joins)
        ));

        for c in &view.columns {
            let ViewColumn::Dimension { name } = c else {
                continue;
            };
            if joined_ds.column(name).is_none() || view.grouping.contains(name) {
                continue;
            }
            let col_grain = joined_ds.column(name).and_then(|c| c.grain());
            selects.push(format!("{alias}.\"{name}\" as \"{name}\""));
            columns.push(CompiledColumn {
                name: name.clone(),
                grain: col_grain,
                attribution_by_depth: (0..=n)
                    .map(|d| match col_grain {
                        Some(g) => attribution_of(g, &view.grouping[..d], dims),
                        None => Attribution::Additive,
                    })
                    .collect(),
                scope_semantics: ScopeSemantics::Direct,
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
        // Shallowest first, so a parent precedes its children.
        format!(" order by s.row_depth asc, {}", keys.join(", "))
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
        grouping: view.grouping.clone(),
        columns,
        stalest_input,
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
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap()
    }

    fn joined_schema() -> SchemaSpec {
        let mut text = String::new();
        text.push_str(
            r#"
[instrument_ref.columns.book]
type = "utf8"
role = "dimension"
[instrument_ref.columns.lhu]
type = "utf8"
role = "dimension"
[instrument_ref.columns.position_ref]
type = "utf8"
role = "key"
[instrument_ref.columns.counterparty]
type = "utf8"
role = "dimension"
[instrument_ref.columns.instrument_ref]
type = "utf8"
role = "key"
[instrument_ref.columns.strike]
type = "f64"
role = "attribute"
grain = "instrument"
"#,
        );
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", &text).unwrap()]);
        let joined = SchemaSpec::from_doc(&doc).0;
        let mut all = schema();
        all.datasets.extend(joined.datasets);
        all
    }

    fn joined_view() -> ViewSpec {
        let text = r#"
[with_ref]
dataset = "risk_snapshot"
grouping = ["instrument_ref"]
[[with_ref.joins]]
dataset = "instrument_ref"
on = ["instrument_ref"]
[[with_ref.columns]]
name = "delta01"
kind = "measure"
[[with_ref.columns]]
name = "strike"
kind = "dimension"
"#;
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        ViewSpec::from_doc(&doc).0.into_iter().next().unwrap()
    }

    #[test]
    fn a_join_puts_reference_columns_on_the_row() {
        let (_d, store) = fixture();
        let schema = joined_schema();
        store
            .apply_schema(schema.dataset("instrument_ref").unwrap())
            .unwrap();
        store
            .writer()
            .execute_batch(
                "insert into instrument_ref_instrument_live
                 values ('BK0','L0','P1','C','I1', 4200.0, 'b', 1, 1, now());",
            )
            .unwrap();

        let q = compile_view(
            store.writer(),
            &joined_view(),
            &schema,
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap();

        let conn = store.writer();
        let mut stmt = conn.prepare(&q.sql).unwrap();
        let strikes: Vec<Option<f64>> = stmt
            .query_map(duckdb::params_from_iter(q.params.iter()), |r| {
                r.get("strike")
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(
            strikes.contains(&Some(4200.0)),
            "the joined strike must reach the row: {strikes:?}"
        );
        // Above the instrument level the key is rolled up to NULL, so the
        // join yields NULL rather than an arbitrary instrument's strike.
        assert!(
            strikes.iter().any(|s| s.is_none()),
            "the total row must not borrow one instrument's strike"
        );
    }

    #[test]
    fn every_dataset_read_is_recorded_for_the_stalest_input_rule() {
        let (_d, store) = fixture();
        let schema = joined_schema();
        store
            .apply_schema(schema.dataset("instrument_ref").unwrap())
            .unwrap();
        let q = compile_view(
            store.writer(),
            &joined_view(),
            &schema,
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap();
        let mut inputs = q.stalest_input.clone();
        inputs.sort();
        assert_eq!(inputs, vec!["instrument_ref", "risk_snapshot"]);
    }

    #[test]
    fn bounding_the_depth_omits_the_levels_below_it() {
        // A collapsed tree must not pay for its leaves: at 1M rows the
        // full tree is 398k rows to display 80 (docs/perf.md), and the
        // blotter's flatten walk is proportional to what was materialized.
        let (_d, store) = fixture();
        let rows_at = |depth: usize| -> Vec<i64> {
            let q = compile_view(
                store.writer(),
                &view(),
                &schema(),
                &Scope::default(),
                &DerivedDimensions::default(),
                &crate::query::as_of::AsOf::Live,
                depth,
            )
            .unwrap();
            let conn = store.writer();
            let mut stmt = conn.prepare(&q.sql).unwrap();
            let depths: Vec<i64> = stmt
                .query_map(duckdb::params_from_iter(q.params.iter()), |r| {
                    r.get("row_depth")
                })
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            depths
        };

        // Depth 0 is the grand total alone.
        assert_eq!(rows_at(0), vec![0]);

        // Depth 1 adds the LHU level and nothing deeper.
        let one = rows_at(1);
        assert!(one.contains(&1), "{one:?}");
        assert!(
            !one.contains(&2) && !one.contains(&3),
            "levels below the bound must not be materialized: {one:?}"
        );

        // The full depth still returns every level, unchanged.
        let all = rows_at(3);
        for d in 0..=3 {
            assert!(all.contains(&d), "depth {d}: {all:?}");
        }
        assert!(all.len() > one.len());
    }

    #[test]
    fn a_bound_past_the_grouping_is_the_whole_tree() {
        // The service passes expanded-depth + 1, which routinely exceeds
        // the grouping — it must not become an out-of-range slice.
        let (_d, store) = fixture();
        let sql = |depth: usize| {
            compile_view(
                store.writer(),
                &view(),
                &schema(),
                &Scope::default(),
                &DerivedDimensions::default(),
                &crate::query::as_of::AsOf::Live,
                depth,
            )
            .unwrap()
            .sql
        };
        assert_eq!(sql(3), sql(usize::MAX));
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
                    r.get("row_depth")?,
                    r.get("delta01")?,
                    r.get("daily_trading_pnl")?,
                ))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        // Grand total, lhu, lhu+underlying (x2), lhu+underlying+position (x2)
        assert_eq!(rows.len(), 6, "{rows:?}");
        assert!(rows.iter().any(|(d, ..)| *d == 0), "grand total missing");
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
                Ok((r.get("row_depth")?, r.get("daily_trading_pnl")?))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        let at = |depth: i64| -> Vec<Option<f64>> {
            rows.iter()
                .filter(|(d, _)| *d == depth)
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
                Ok((r.get("row_depth")?, r.get("delta01")?))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let total: Option<f64> = rows.iter().find(|(d, _)| *d == 0).map(|(_, v)| *v).unwrap();
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
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
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
