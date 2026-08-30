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

/// Single-quote escaping for a literal inlined into SQL. Derived
/// dimension values come from config, which is trusted but not
/// necessarily quote-free.
fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// A derived dimension as a scalar expression over its source column
/// (spec §6.8): `case "book" when 'BK000' then 'IDX_EXO_EU' ... end`.
///
/// A scalar projection rather than a joined lookup table on purpose. A
/// join can duplicate rows when the key repeats and can match NULL keys
/// against rolled-up levels — both defects this compiler has had — while
/// a `case` is a pure function of the row: it cannot change cardinality,
/// and an unmapped value falls through to NULL, which is the honest
/// answer for a book the desk map does not cover.
fn derived_expr(d: &geode_core::dimensions::DerivedDimension) -> String {
    if d.values.is_empty() {
        return format!("NULL::varchar as \"{}\"", d.name);
    }
    let arms: Vec<String> = d
        .values
        .iter()
        .map(|(source, derived)| {
            format!("when {} then {}", sql_literal(source), sql_literal(derived))
        })
        .collect();
    format!(
        "case \"{from}\" {arms} end as \"{name}\"",
        from = d.from,
        arms = arms.join(" "),
        name = d.name
    )
}

/// The scanned relation, with any derived dimensions this query groups by
/// projected onto it. Wrapping the table rather than rewriting every
/// reference keeps `group by`, `grouping()` and the scope's `base` alias
/// working on a plain column name.
fn scan(table: &str, derived: &[&geode_core::dimensions::DerivedDimension]) -> String {
    if derived.is_empty() {
        return table.to_string();
    }
    format!(
        "(select *, {} from {table})",
        derived
            .iter()
            .map(|d| derived_expr(d))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Derived dimensions among `columns` whose source column exists at
/// `grain`. A dimension whose source is not on the table would compile to
/// a `case` over a column that is not there.
fn derived_for<'a>(
    columns: &[String],
    dims: &'a DerivedDimensions,
    grain: Grain,
) -> Vec<&'a geode_core::dimensions::DerivedDimension> {
    columns
        .iter()
        .filter_map(|c| dims.get(c))
        .filter(|d| grain.key_columns().contains(&d.from.as_str()))
        .collect()
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
    // One era for the whole statement: the spine, every aggregate, and
    // the scope's semi-join probe must all read the same tables.
    let era = crate::query::scope_sql::Era {
        kind: kind_for_joins,
        generations: gen_pred.as_deref(),
    };
    let and_gen = |p: &str| match &gen_pred {
        Some(g) => format!("({p}) and ({g})"),
        None => p.to_string(),
    };
    let spine_scope = compile_scope(conn, scope, ds, spine_grain, dims, spine_grain, era)?;
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
             from {table} base where {pred})",
            table = scan(
                &table_name(&view.dataset, spine_grain, kind_for_joins),
                &derived_for(&view.grouping, dims, spine_grain),
            ),
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
             from {table} base where {pred} group by {sets})",
            select = materialized.join(", "),
            group = materialized.join(", "),
            table = scan(
                &table_name(&view.dataset, spine_grain, kind_for_joins),
                &derived_for(&view.grouping[..depth], dims, spine_grain),
            ),
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
        let grain_scope = compile_scope(conn, scope, ds, grain, dims, spine_grain, era)?;

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

        // How many of *this grain's* grouping columns are present at each
        // spine depth. The aggregate groups by the same prefixes projected
        // onto the columns it has, so this is the map between the spine's
        // depth and the aggregate's own level.
        let own_present: Vec<usize> = (0..=depth)
            .map(|d| {
                own.iter()
                    .filter(|c| view.grouping[..d].contains(c))
                    .count()
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
        // The aggregate carries its own level, for the same reason the
        // spine does: matching on key values alone cannot tell a
        // rolled-up NULL from a NULL that is really in the data, so a
        // single NULL `lhu` would attach the aggregate's higher levels to
        // the leaf row and fan it out.
        let sub_depth = if own.is_empty() {
            "0 as sub_depth".to_string()
        } else {
            format!(
                "({} - bit_count(grouping({}))) as sub_depth",
                own.len(),
                own_q.join(", ")
            )
        };
        ctes.push(format!(
            "{alias} as (select {keys}{comma}{sub_depth}, {aggs} \
             from {table} base where {pred}{sub_group})",
            keys = own_q.join(", "),
            comma = if own.is_empty() { "" } else { ", " },
            aggs = aggs.join(", "),
            table = scan(
                &table_name(&view.dataset, grain, kind_for_joins),
                &derived_for(&own, dims, grain),
            ),
            pred = and_gen(&grain_scope.predicate),
        ));
        // The grain subquery's params follow the spine's, in CTE order.
        params.extend(grain_scope.params);

        let on = if own.is_empty() {
            "true".to_string()
        } else {
            let level = format!(
                "{alias}.sub_depth = case s.row_depth {} else 0 end",
                own_present
                    .iter()
                    .enumerate()
                    .map(|(d, present)| format!("when {d} then {present}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            own.iter()
                .map(|c| format!("{alias}.\"{c}\" is not distinct from s.\"{c}\""))
                .chain(std::iter::once(level))
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

        // The key must be on the spine *as materialized*. Testing the
        // whole grouping would reference a column the bounded spine does
        // not group by, which is a binder error rather than a NULL.
        if !join.on.iter().all(|k| view.grouping[..depth].contains(k)) {
            continue;
        }
        let Some(joined_grain) = joined_ds.grains().into_iter().find(|g| {
            join.on
                .iter()
                .all(|k| g.key_columns().contains(&k.as_str()))
        }) else {
            continue;
        };
        // Only once the join is known to actually happen: a skipped join
        // must not make the view report itself as stale as a dataset it
        // never read (spec §5.4).
        stalest_input.push(join.dataset.clone());

        // Which of the joined dataset's columns this view actually wants.
        let wanted: Vec<&String> = view
            .columns
            .iter()
            .filter_map(|c| match c {
                ViewColumn::Dimension { name } => Some(name),
                _ => None,
            })
            .filter(|name| joined_ds.column(name).is_some() && !view.grouping.contains(*name))
            .collect();

        // A joined dataset's table is keyed finer than the join key — an
        // instrument's reference row exists per position that holds it —
        // so joining the table directly multiplies every spine row by how
        // many rows share the key. Aggregating to the join key first is
        // what makes this a lookup rather than a fan-out. `any_value` is
        // the right reducer because these are attributes that should
        // agree; where they do not, that is what the cross-file conflict
        // detector reports (spec §3.5), not something to average.
        let joined_gen = match as_of {
            crate::query::as_of::AsOf::Live => None,
            crate::query::as_of::AsOf::At(t) => {
                // Each dataset has its own generations, so the spine's
                // predicate does not apply here. Without this a
                // historical join reads every archived generation at once.
                let archive = table_name(&join.dataset, joined_grain, TableKind::Archive);
                let gens = crate::query::as_of::resolve_generations(conn, &archive, *t)?;
                Some(crate::query::as_of::generation_predicate(&gens))
            }
        };
        let projection: Vec<String> = join
            .on
            .iter()
            .map(|k| format!("\"{k}\""))
            .chain(
                wanted
                    .iter()
                    .map(|name| format!("any_value(\"{name}\") as \"{name}\"")),
            )
            .collect();

        let alias = format!("join_{i}");
        // Plain `=`, not `is not distinct from`: above the join's level
        // the spine has already NULLed the key, and NULL must not match.
        // `is not distinct from` would attach a reference row whose key is
        // NULL to every rolled-up row above it.
        let on = join
            .on
            .iter()
            .map(|k| format!("{alias}.\"{k}\" = s.\"{k}\""))
            .collect::<Vec<_>>()
            .join(" and ");
        joins.push(format!(
            "left join (select {projection} from {table} where {pred} group by {keys}) {alias} on {on}",
            projection = projection.join(", "),
            table = table_name(&join.dataset, joined_grain, kind_for_joins),
            pred = joined_gen.as_deref().unwrap_or("true"),
            keys = join
                .on
                .iter()
                .map(|k| format!("\"{k}\""))
                .collect::<Vec<_>>()
                .join(", "),
        ));

        for name in wanted {
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

    /// The joined view over a store that already has the reference table.
    fn joined_query(
        store: &crate::store::Store,
        schema: &SchemaSpec,
        max_depth: usize,
    ) -> CompiledQuery {
        compile_view(
            store.writer(),
            &joined_view(),
            schema,
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            max_depth,
        )
        .unwrap()
    }

    #[test]
    fn a_reference_row_per_holder_does_not_multiply_the_spine() {
        // The reference table is keyed per position, so one instrument
        // held by two positions has two rows. Joining the table directly
        // would duplicate every spine row that matched — and duplicate its
        // measures with it.
        let (_d, store) = fixture();
        let schema = joined_schema();
        store
            .apply_schema(schema.dataset("instrument_ref").unwrap())
            .unwrap();
        store
            .writer()
            .execute_batch(
                "insert into instrument_ref_instrument_live values
                   ('BK0','L0','P1','C','I1', 4200.0, 'b', 1, 1, now()),
                   ('BK0','L0','P2','C','I1', 4200.0, 'b', 1, 1, now());",
            )
            .unwrap();

        let q = joined_query(&store, &schema, usize::MAX);
        let rows = run(&store, &q, &["row_depth", "instrument_ref", "strike"]);
        let mut deduped = rows.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(
            rows.len(),
            deduped.len(),
            "two reference rows for one instrument duplicated the spine: {rows:?}"
        );
        assert_eq!(rows.len(), 2, "grand total and one instrument: {rows:?}");
    }

    #[test]
    fn a_null_reference_key_does_not_attach_to_every_rolled_up_row() {
        // `is not distinct from` would make a NULL key match the NULLs a
        // rolled-up level carries, so the grand total would borrow this
        // row's strike and read as though it belonged to one instrument.
        let (_d, store) = fixture();
        let schema = joined_schema();
        store
            .apply_schema(schema.dataset("instrument_ref").unwrap())
            .unwrap();
        store
            .writer()
            .execute_batch(
                "insert into instrument_ref_instrument_live values
                   ('BK0','L0','P1','C', NULL, 9999.0, 'b', 1, 1, now());",
            )
            .unwrap();

        let q = joined_query(&store, &schema, usize::MAX);
        let rows = run(&store, &q, &["row_depth", "strike"]);
        let total: Vec<&Vec<String>> = rows.iter().filter(|r| r[0] == "Some(0.0)").collect();
        assert_eq!(total.len(), 1, "one grand total: {rows:?}");
        assert_eq!(
            total[0][1], "None",
            "the total must not borrow a NULL-keyed strike: {rows:?}"
        );
    }

    #[test]
    fn a_joined_view_bounded_to_the_total_still_compiles_and_runs() {
        // At depth 0 the spine groups by nothing, so a join key tested
        // against the *full* grouping would reference a column that is not
        // there — a binder error, not a NULL.
        let (_d, store) = fixture();
        let schema = joined_schema();
        store
            .apply_schema(schema.dataset("instrument_ref").unwrap())
            .unwrap();
        let q = joined_query(&store, &schema, 0);
        let rows = run(&store, &q, &["row_depth"]);
        assert_eq!(rows.len(), 1, "the grand total alone: {rows:?}");
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

    /// Run a compiled query and return the rows as `(row_depth, values)`.
    /// The tests that matter here are about what the database *does*, not
    /// what the SQL string looks like.
    fn run(store: &crate::store::Store, q: &CompiledQuery, columns: &[&str]) -> Vec<Vec<String>> {
        let conn = store.writer();
        let mut stmt = conn
            .prepare(&q.sql)
            .unwrap_or_else(|e| panic!("prepare failed: {e}\n{}", q.sql));
        let rows = stmt
            .query_map(duckdb::params_from_iter(q.params.iter()), |r| {
                Ok(columns
                    .iter()
                    .map(|c| {
                        r.get::<_, Option<f64>>(*c)
                            .map(|v| format!("{v:?}"))
                            .or_else(|_| r.get::<_, Option<i64>>(*c).map(|v| format!("{v:?}")))
                            .or_else(|_| r.get::<_, Option<String>>(*c).map(|v| format!("{v:?}")))
                            .unwrap_or_else(|_| "?".into())
                    })
                    .collect::<Vec<String>>())
            })
            .unwrap_or_else(|e| panic!("execute failed: {e}\n{}", q.sql));
        rows.map(|r| r.unwrap()).collect()
    }

    #[test]
    fn a_real_null_in_a_grouping_column_does_not_fan_out_the_tree() {
        // A rolled-up level carries NULL in the columns below it, so
        // matching an aggregate on key values alone cannot tell that NULL
        // apart from one that is genuinely in the data — and the higher
        // aggregate levels then attach to the leaf as well, duplicating
        // rows and double-counting. Books with no LHU are ordinary in the
        // real feed (2a reports them rather than dropping them).
        let (_d, store) = fixture();
        store
            .writer()
            .execute_batch(
                "insert into risk_snapshot_position_live values
                   ('BK0',NULL,'P9','C', 5, 'b', 1, 1, now());
                 insert into risk_snapshot_underlying_live values
                   ('BK0',NULL,'P9','C','I9','SPX', 3, 'b', 1, 1, now());",
            )
            .unwrap();

        let q = compile(&store);
        // Every grouping column, or legitimately distinct rows would look
        // like duplicates and the assertion would be about the wrong thing.
        let rows = run(
            &store,
            &q,
            &["row_depth", "lhu", "underlying_ref", "position_ref"],
        );

        // Every level must appear exactly once per group.
        let mut seen = rows.clone();
        seen.sort();
        let mut deduped = seen.clone();
        deduped.dedup();
        assert_eq!(
            seen, deduped,
            "the same level appeared more than once — the join fanned out"
        );

        // And the grand total must still be the sum of the leaves, not a
        // multiple of it: 7 + 5 across the two positions.
        let totals = run(&store, &q, &["row_depth", "daily_trading_pnl"]);
        let grand: Vec<&Vec<String>> = totals.iter().filter(|r| r[0] == "Some(0.0)").collect();
        assert_eq!(grand.len(), 1, "one grand total row: {totals:?}");
        assert_eq!(grand[0][1], "Some(12.0)", "7 + 5, not doubled: {totals:?}");
    }

    /// `desk` derived from `book` — the standing §6.8 case.
    fn desks() -> DerivedDimensions {
        let doc = geode_core::config::merge_docs(
            "dimensions",
            &[geode_core::config::LayerDoc::builtin(
                "dimensions",
                "[desk]\nfrom = \"book\"\n[desk.values]\nEU = [\"BK0\"]\nUS = [\"BK9\"]\n",
            )
            .unwrap()],
        );
        DerivedDimensions::from_doc(&doc).0
    }

    fn desk_view() -> ViewSpec {
        let text = r#"
[by_desk]
dataset = "risk_snapshot"
grouping = ["desk"]
[[by_desk.columns]]
name = "delta01"
kind = "measure"
"#;
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        ViewSpec::from_doc(&doc).0.into_iter().next().unwrap()
    }

    #[test]
    fn grouping_by_a_derived_dimension_produces_its_mapped_values() {
        // The map lives in config and has to reach the SQL: without it the
        // spine groups by a column no table has.
        let (_d, store) = fixture();
        let q = compile_view(
            store.writer(),
            &desk_view(),
            &schema(),
            &Scope::default(),
            &desks(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap();
        let rows = run(&store, &q, &["row_depth", "desk", "delta01"]);
        assert!(
            rows.iter()
                .any(|r| r[0] == "Some(1.0)" && r[1] == "Some(\"EU\")"),
            "BK0 must roll up under desk EU: {rows:?}"
        );
        // 10 + 20 across the fixture's two underlyings, under one desk.
        let eu = rows.iter().find(|r| r[1] == "Some(\"EU\")").unwrap();
        assert_eq!(eu[2], "Some(30.0)", "{rows:?}");
    }

    #[test]
    fn scoping_by_a_derived_dimension_translates_back_to_source_values() {
        // The silent one: the stored column holds `book`, so binding the
        // derived value 'EU' against it compiles fine and matches nothing.
        // An empty result is indistinguishable from a real empty result.
        let (_d, store) = fixture();
        let scope = Scope {
            dimensions: vec![geode_core::scope::DimensionSelection {
                column: "desk".into(),
                values: vec!["EU".into()],
            }],
            ..Scope::default()
        };
        let q = compile_view(
            store.writer(),
            &desk_view(),
            &schema(),
            &scope,
            &desks(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap();
        let rows = run(&store, &q, &["row_depth", "delta01"]);
        let total = rows.iter().find(|r| r[0] == "Some(0.0)").unwrap();
        assert_eq!(
            total[1], "Some(30.0)",
            "scoping to desk EU must keep BK0's rows: {rows:?}"
        );

        // And a desk the map does not produce selects nothing, rather
        // than everything.
        let unknown = Scope {
            dimensions: vec![geode_core::scope::DimensionSelection {
                column: "desk".into(),
                values: vec!["NOWHERE".into()],
            }],
            ..Scope::default()
        };
        let q = compile_view(
            store.writer(),
            &desk_view(),
            &schema(),
            &unknown,
            &desks(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap();
        let rows = run(&store, &q, &["row_depth", "delta01"]);
        let total = rows.iter().find(|r| r[0] == "Some(0.0)").unwrap();
        assert_eq!(total[1], "None", "an unmapped desk selects nothing");
    }

    #[test]
    fn an_as_of_query_reads_only_the_archive_even_through_a_semi_join() {
        // The whole as-of compile path had no test, which is how two
        // silent defects survived: the semi-join probe read `_live` with
        // no generation predicate, so a historical answer quietly mixed in
        // today's numbers (spec §6.5).
        let (_d, store) = fixture();
        let conn = store.writer();
        // Two generations in the archive, and a *different* value live —
        // if live leaks in, the totals move.
        conn.execute_batch(
            "insert into risk_snapshot_position_archive
               select 'BK0','L0','P1','C', 100, 'b', 1, 1, TIMESTAMPTZ '2026-08-01 00:00:00Z';
             insert into risk_snapshot_underlying_archive
               select 'BK0','L0','P1','C','I1','SPX', 40, 'b', 1, 1,
                      TIMESTAMPTZ '2026-08-01 00:00:00Z';",
        )
        .unwrap();

        let at = chrono::DateTime::parse_from_rfc3339("2026-08-15T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let scope = Scope {
            dimensions: vec![geode_core::scope::DimensionSelection {
                column: "underlying_ref".into(),
                values: vec!["SPX".into()],
            }],
            ..Scope::default()
        };
        let q = compile_view(
            store.writer(),
            &view(),
            &schema(),
            &scope,
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::At(at),
            usize::MAX,
        )
        .unwrap();

        assert!(
            !q.sql.contains("_live"),
            "an as-of query must not name a live table: {}",
            q.sql
        );
        let rows = run(&store, &q, &["row_depth", "daily_trading_pnl"]);
        let total = rows.iter().find(|r| r[0] == "Some(0.0)").unwrap();
        assert_eq!(
            total[1], "Some(100.0)",
            "the archived value, not the live 7: {rows:?}"
        );
    }

    #[test]
    fn a_null_key_still_satisfies_its_own_semi_join() {
        // The semi-join matches a table against itself, so a NULL key is a
        // real value on both sides — not the rolled-up placeholder the
        // spine uses. Plain equality would drop the coarse measure while
        // leaving the row visible, which reads as an inconsistency rather
        // than an absence.
        let (_d, store) = fixture();
        store
            .writer()
            .execute_batch(
                "insert into risk_snapshot_position_live values
                   ('BK0',NULL,'P9','C', 5, 'b', 1, 1, now());
                 insert into risk_snapshot_underlying_live values
                   ('BK0',NULL,'P9','C','I9','SPX', 3, 'b', 1, 1, now());",
            )
            .unwrap();

        let scope = Scope {
            dimensions: vec![geode_core::scope::DimensionSelection {
                column: "underlying_ref".into(),
                values: vec!["SPX".into()],
            }],
            ..Scope::default()
        };
        let q = compile_view(
            store.writer(),
            &view(),
            &schema(),
            &scope,
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap();
        let rows = run(&store, &q, &["row_depth", "daily_trading_pnl"]);
        let total = rows.iter().find(|r| r[0] == "Some(0.0)").unwrap();
        assert_eq!(
            total[1], "Some(12.0)",
            "the NULL-LHU position's 5 must count too: {rows:?}"
        );
    }

    #[test]
    fn a_derived_dimension_works_in_the_expression_grammar_too() {
        // `desk = 'EU'` is a natural thing to type. Before this it either
        // named a column no table has, or bound a derived value against
        // the source column and matched nothing.
        let (_d, store) = fixture();
        let scoped = |text: &str| {
            let q = compile_view(
                store.writer(),
                &view(),
                &schema(),
                &Scope {
                    expression: Some(geode_core::scope::parse_expr(text).unwrap()),
                    ..Scope::default()
                },
                &desks(),
                &crate::query::as_of::AsOf::Live,
                usize::MAX,
            )
            .unwrap();
            let rows = run(&store, &q, &["row_depth", "delta01"]);
            rows.iter()
                .find(|r| r[0] == "Some(0.0)")
                .map(|r| r[1].clone())
                .unwrap()
        };
        assert_eq!(scoped("desk = 'EU'"), "Some(30.0)", "BK0 is desk EU");
        assert_eq!(scoped("desk != 'EU'"), "None", "nothing else is loaded");
        assert_eq!(scoped("desk in ('EU', 'US')"), "Some(30.0)");
        assert_eq!(scoped("desk = 'NOWHERE'"), "None", "unmapped selects none");

        // A derived dimension has no order of its own, so an ordering
        // comparison is rejected rather than silently compared against
        // whatever the map happens to spell.
        let err = compile_view(
            store.writer(),
            &view(),
            &schema(),
            &Scope {
                expression: Some(geode_core::scope::parse_expr("desk > 'EU'").unwrap()),
                ..Scope::default()
            },
            &desks(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        );
        assert!(err.is_err(), "ordering on a derived dimension must fail");
    }

    #[test]
    fn a_scope_finer_than_the_measure_grain_actually_executes() {
        // The semi-join path: `daily_trading_pnl` lives at position grain,
        // but the scope names `underlying_ref`, which does not exist
        // there. Asserting the SQL merely *contains* "exists" would pass
        // on SQL the database rejects, so this runs it.
        let (_d, store) = fixture();
        let scope = Scope {
            dimensions: vec![geode_core::scope::DimensionSelection {
                column: "underlying_ref".into(),
                values: vec!["SPX".into()],
            }],
            ..Scope::default()
        };
        let q = compile_view(
            store.writer(),
            &view(),
            &schema(),
            &scope,
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap();
        let rows = run(&store, &q, &["row_depth"]);
        assert!(!rows.is_empty(), "the scoped query returned nothing");
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
