//! The view compiler (spec §6.3). One statement per view, covering every
//! level of the rollup tree, with each measure aggregated at its own grain
//! and joined at group cardinality.
//!
//! Emitting one query per level would put tree navigation on the 50ms
//! requery budget; emitting one puts expand and collapse on the 8ms frame
//! budget instead, because every level is already in the snapshot.

use crate::query::scope_sql::{Era, compile_scope};
use crate::store::StoreError;
use crate::store::ddl::{TableKind, table_name};
use duckdb::Connection;
use duckdb::types::Value;
use geode_core::attribution::{Attribution, ScopeSemantics, attribution_of};
use geode_core::dimensions::DerivedDimensions;
use geode_core::schema::{Aggregate, ColumnRole, ColumnSpec, DatasetSpec, Grain, SchemaSpec};
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
    /// For an as-of query, the *oldest* generation actually resolved per
    /// dataset — the same stalest-input rule live freshness uses (§4.5: a
    /// dataset's headline as-of is its oldest book). The *requested*
    /// instant is not the answer: asking for today against data last
    /// published a month ago must report the month-old time, or §5.4's
    /// stalest-input rule reports nothing at all because every dataset
    /// carries the same requested value.
    pub resolved_as_of: std::collections::BTreeMap<String, chrono::DateTime<chrono::Utc>>,
}

fn quoted(cols: &[String]) -> Vec<String> {
    cols.iter().map(|c| format!("\"{c}\"")).collect()
}

/// Whether `grain` carries every one of `columns` as a dimension —
/// derived dimensions resolved to their source first.
fn carries_all(grain: Grain, columns: &[String], dims: &DerivedDimensions) -> bool {
    columns.iter().all(|col| {
        grain
            .dimension_key_columns()
            .contains(&dims.base_column(col))
    })
}

/// The finest declared grain carrying every one of `columns`.
///
/// It must be *declared* — `apply_schema` only creates tables for grains
/// the dataset has measures or attributes at, so picking the finest grain
/// unconditionally would name a table that does not exist. Dimension
/// keys, not the raw key: the pair table's `underlying_ref` is `least(u1,
/// u2)`, and a tree spined on it would show only the underlying that
/// sorts first in each pair.
fn finest_carrying(
    ds: &DatasetSpec,
    columns: &[String],
    dims: &DerivedDimensions,
) -> Option<Grain> {
    ds.grains()
        .into_iter()
        .rev()
        .find(|g| carries_all(*g, columns, dims))
}

/// The already-compiled columns a derived expression names.
///
/// The expression is raw SQL, not the scope grammar, so there is no parse
/// tree to walk. Identifier-like tokens are matched against the columns
/// the view has already produced, which is exactly the set a derived
/// column is allowed to reference. Tokenising rather than substring
/// matching is what keeps `delta01` out of `delta01_usd`, and skipping
/// quoted text keeps a column name inside a string literal from counting.
///
/// Over-matching is the safe direction: a name that appears as a SQL
/// keyword or function would only pull in a *weaker* marker, never a
/// stronger one, and none of the aggregate names collide with the
/// column vocabulary here.
fn referenced_columns<'a>(sql: &str, columns: &'a [CompiledColumn]) -> Vec<&'a CompiledColumn> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    for ch in sql.chars() {
        if ch == '\'' {
            in_string = !in_string;
            current.clear();
            continue;
        }
        if in_string {
            continue;
        }
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch);
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }

    columns
        .iter()
        .filter(|c| tokens.contains(&c.name))
        .collect()
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
/// projected onto it. Wrapping the relation rather than rewriting every
/// reference keeps `group by`, `grouping()` and the scope's `base` alias
/// working on a plain column name.
fn scan(relation: &str, derived: &[&geode_core::dimensions::DerivedDimension]) -> String {
    if derived.is_empty() {
        return relation.to_string();
    }
    format!(
        "(select *, {} from {relation})",
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
        .filter(|d| grain.dimension_key_columns().contains(&d.from.as_str()))
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
    rows.collect::<Result<Vec<_>, _>>().map_err(err)
}

/// Every table a dataset's history lives in: the archive **and** live for
/// each grain. Generations are resolved across all of them — a partition
/// can be missing from one grain while present at another (a cash-only
/// book has no underlying rows), and the generation a partition holds now
/// is in live and nowhere else (see `Era::relation`).
fn history_of(dataset: &str, ds: &DatasetSpec) -> Vec<String> {
    ds.grains()
        .into_iter()
        .flat_map(|g| {
            [
                table_name(dataset, g, TableKind::Archive),
                table_name(dataset, g, TableKind::Live),
            ]
        })
        .collect()
}

fn compile_error(view: &ViewSpec, message: String) -> StoreError {
    StoreError::Sql {
        statement: format!("compile view '{}'", view.name),
        source: duckdb::Error::InvalidParameterName(message),
    }
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
        .ok_or_else(|| compile_error(view, format!("unknown dataset '{}'", view.dataset)))?;

    let n = view.grouping.len();
    let depth = max_depth.min(n);
    let group_cols = quoted(&view.grouping);
    let materialized = &view.grouping[..depth];
    let mut params: Vec<Value> = Vec::new();
    let mut ctes: Vec<String> = Vec::new();
    let mut selects: Vec<String> = Vec::new();
    let mut joins: Vec<String> = Vec::new();
    let mut columns: Vec<CompiledColumn> = Vec::new();

    // Same statement shape, different relations (spec §6.5). Only the
    // as-of path pays for history; live carries no generation predicate
    // at all. Resolved across every table the dataset's history lives in,
    // not one grain's: a partition can be missing from one grain while
    // present at another, and resolving from one would drop it from every
    // grain's answer (see `resolve_generations`).
    let mut resolved_as_of: std::collections::BTreeMap<String, chrono::DateTime<chrono::Utc>> =
        std::collections::BTreeMap::new();
    let (kind, gen_pred) = match as_of {
        crate::query::as_of::AsOf::Live => (TableKind::Live, None),
        crate::query::as_of::AsOf::At(t) => {
            let gens =
                crate::query::as_of::resolve_generations(conn, &history_of(&view.dataset, ds), *t)?;
            if let Some(oldest) = gens.iter().map(|g| g.source_time).min() {
                resolved_as_of.insert(view.dataset.clone(), oldest);
            }
            (
                TableKind::Archive,
                Some(crate::query::as_of::generation_predicate(&gens)),
            )
        }
    };
    // One era for the whole statement: every aggregate, the spine's
    // fallback scan, the scope's membership probes and the cross-dataset
    // joins must all read the same relations.
    let era = Era {
        kind,
        generations: gen_pred.as_deref(),
    };
    let and_gen = |p: &str| match &gen_pred {
        Some(g) => format!("({p}) and ({g})"),
        None => p.to_string(),
    };

    // The spine is the set of `(grouping tuple, depth)` rows the tree has,
    // and it is assembled from the aggregates rather than scanned from one
    // table. A spine scanned from the finest table only has rows for
    // entities that table holds — a cash-only book has position rows and
    // no underlying rows, so it was in the grand total and on no row
    // beneath it, and the children did not sum to their parent. Each
    // aggregate already knows its groups at every level it carries; the
    // spine is their union, and the grand-total row is a constant so a
    // scope selecting nothing still yields the one row a tree always has.
    let mut spine_sources: Vec<String> = vec![if depth == 0 {
        "select 0 as row_depth".to_string()
    } else {
        format!(
            "select {}, 0 as row_depth",
            materialized
                .iter()
                .map(|g| format!("NULL as \"{g}\""))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }];
    let mut covered = vec![false; depth + 1];
    covered[0] = true;

    // Deferred until the spine is in `ctes`: the outer select's joins and
    // columns, in view order.
    let mut agg_joins: Vec<String> = Vec::new();
    let mut agg_selects: Vec<String> = Vec::new();
    let mut agg_columns: Vec<CompiledColumn> = Vec::new();

    // One aggregate subquery per measure grain the view touches.
    for grain in view.measure_grains(schema) {
        let alias = format!("agg_{}", grain.table());
        let grain_scope = compile_scope(conn, scope, ds, grain, dims, era)?;

        // Only the grouping columns this grain carries, and only within
        // the materialized depth — selecting a key the spine no longer
        // groups by would leave it outside every aggregate.
        let own: Vec<String> = materialized
            .iter()
            .filter(|g| grain.dimension_key_columns().contains(&dims.base_column(g)))
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
             from {relation} base where {pred}{sub_group})",
            keys = own_q.join(", "),
            comma = if own.is_empty() { "" } else { ", " },
            aggs = aggs.join(", "),
            relation = scan(
                &era.relation(&view.dataset, grain),
                &derived_for(&own, dims, grain),
            ),
            pred = and_gen(&grain_scope.predicate),
        ));
        // The grain subquery's params follow the previous CTE's, in CTE
        // order.
        params.extend(grain_scope.params);

        // The depths this grain carries in full are spine levels it can
        // supply: at such a depth its own level *is* the spine's.
        let carried: Vec<usize> = (1..=depth).filter(|d| own_present[*d] == *d).collect();
        if !carried.is_empty() {
            let projection: Vec<String> = materialized
                .iter()
                .map(|g| {
                    if own.contains(g) {
                        format!("{alias}.\"{g}\"")
                    } else {
                        "NULL".to_string()
                    }
                })
                .chain(std::iter::once(format!("{alias}.sub_depth")))
                .collect();
            spine_sources.push(format!(
                "select {} from {alias} where {alias}.sub_depth in ({})",
                projection.join(", "),
                carried
                    .iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            for d in carried {
                covered[d] = true;
            }
        }

        let on = if own.is_empty() {
            "true".to_string()
        } else {
            // `else -1`: unreachable, and a row that reached it would
            // match nothing rather than the grand total.
            let level = format!(
                "{alias}.sub_depth = case s.row_depth {} else -1 end",
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
        agg_joins.push(format!("left join {alias} on {on}"));

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
            agg_selects.push(format!("{expr} as \"{}\"", m.name));
            agg_columns.push(CompiledColumn {
                name: m.name.clone(),
                grain: Some(grain),
                attribution_by_depth: by_depth,
                scope_semantics: grain_scope.semantics.clone(),
            });
        }
    }

    // Depths no measure grain carries — a view with no measures, or one
    // grouped by a column finer than every measure it shows — are scanned
    // from the finest declared grain that carries them, exactly as the
    // whole spine once was.
    let missing: Vec<usize> = (1..=depth).filter(|d| !covered[*d]).collect();
    if !missing.is_empty() {
        let spine_grain = finest_carrying(ds, materialized, dims).ok_or_else(|| {
            compile_error(
                view,
                format!(
                    "no declared grain of '{}' carries every grouping column {:?}; \
                     a grouping column must be a dimension key of some grain",
                    view.dataset, materialized
                ),
            )
        })?;
        let spine_scope = compile_scope(conn, scope, ds, spine_grain, dims, era)?;
        // `grouping()` may only name columns some set groups by, and its
        // width would then change with the bound — so the scan emits an
        // explicit depth instead of a bitmask. Under prefix sets a level
        // with `p` of `depth` columns present sets the top `depth - p`
        // bits, so popcount recovers the depth directly.
        let sets: Vec<String> = missing
            .iter()
            .map(|d| format!("({})", group_cols[..*d].join(", ")))
            .collect();
        spine_sources.push(format!(
            "select {select}, ({depth} - bit_count(grouping({select}))) as row_depth \
             from {relation} base where {pred} group by grouping sets ({sets})",
            select = group_cols[..depth].join(", "),
            relation = scan(
                &era.relation(&view.dataset, spine_grain),
                &derived_for(materialized, dims, spine_grain),
            ),
            pred = and_gen(&spine_scope.predicate),
            sets = sets.join(", "),
        ));
        params.extend(spine_scope.params);
    }
    ctes.push(format!(
        "spine as (select distinct * from ({}))",
        spine_sources.join(" union all ")
    ));

    // Cast only to types that actually exist: the derived ENUMs are built
    // by ingest, so before the first load there are none and the cast
    // would be a hard error. Degrading to plain strings is correct — the
    // interning is an optimization, not a semantic.
    //
    // And only for live. The ENUMs are rebuilt from the *live* table on
    // every ingest, so they carry today's values; an archived row holding
    // a value that has since left live — a retired book, a closed LHU —
    // cannot be cast through them, and the query fails outright with a
    // conversion error. The era decides this, like every other relation
    // choice in this function.
    let interned: Vec<&str> = if era.kind != TableKind::Live {
        Vec::new()
    } else {
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
        } else if ty == "varchar" {
            selects.push(format!("s.\"{g}\"::{ty} as \"{g}\""));
        } else {
            // `try_cast`, not `::`. The ENUM is rebuilt at ingest (§3.6),
            // so a value it does not carry means something already went
            // wrong upstream — but a plain cast turns that anomaly into
            // `Conversion Error: Could not convert string 'X' to UINT8`,
            // which fails the whole statement. One unknown book then costs
            // the trader every row of the tile rather than one cell.
            //
            // The cost is that such a value reads back blank, and a blank
            // dimension cell already means "rolled up". That is a real
            // ambiguity and it is the lesser one: the alternative is not a
            // louder error about that value, it is no data at all.
            selects.push(format!("try_cast(s.\"{g}\" as {ty}) as \"{g}\""));
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
    joins.extend(agg_joins);
    selects.extend(agg_selects);
    columns.extend(agg_columns);

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
        if !join.on.iter().all(|k| materialized.contains(k)) {
            continue;
        }
        let Some(joined_grain) = joined_ds
            .grains()
            .into_iter()
            .find(|g| carries_all(*g, &join.on, dims))
        else {
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
                let gens = crate::query::as_of::resolve_generations(
                    conn,
                    &history_of(&join.dataset, joined_ds),
                    *t,
                )?;
                if let Some(oldest) = gens.iter().map(|g| g.source_time).min() {
                    resolved_as_of.insert(join.dataset.clone(), oldest);
                }
                Some(crate::query::as_of::generation_predicate(&gens))
            }
        };
        let joined_era = Era {
            kind,
            generations: joined_gen.as_deref(),
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
            "left join (select {projection} from {relation} where {pred} group by {keys}) {alias} on {on}",
            projection = projection.join(", "),
            relation = joined_era.relation(&join.dataset, joined_grain),
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
            // A derived column is only as attributable as what it is
            // computed from. Marked Additive/Direct unconditionally, an
            // expression over a NonAttributable measure claimed to be
            // summable at a level where its own input is blanked (§6.3) —
            // and the marker is the only thing a renderer has to go on, so
            // a wrong one is worse than a missing column.
            let referenced = referenced_columns(sql, &columns);
            let (attribution_by_depth, scope_semantics) = if referenced.is_empty() {
                // A constant, or an expression over nothing this view
                // selects: nothing to inherit, and nothing to overclaim.
                (vec![Attribution::Additive; n + 1], ScopeSemantics::Direct)
            } else {
                let meet_at = |depth: usize| {
                    referenced
                        .iter()
                        .filter_map(|c| c.attribution_by_depth.get(depth).copied())
                        .fold(Attribution::Additive, Attribution::meet)
                };
                (
                    (0..=n).map(meet_at).collect(),
                    referenced.iter().fold(ScopeSemantics::Direct, |acc, c| {
                        acc.meet(&c.scope_semantics)
                    }),
                )
            };
            columns.push(CompiledColumn {
                name: name.clone(),
                grain: None,
                attribution_by_depth,
                scope_semantics,
            });
        }
    }

    // Shallowest first, so a parent always precedes its children — the
    // order a flatten walk over the rows assumes. This is emitted whether
    // or not the view declares a sort: with no ORDER BY at all the rows
    // arrive in whatever order the plan produces, which puts the grand
    // total in the middle of the result and a child before its parent.
    //
    // The grouping columns follow as tie-breakers so the order is total.
    // Without them two runs of one query can interleave a depth's rows
    // differently, and a tile that requeries every few seconds reshuffles
    // rows that did not change.
    //
    // This is a determinism backstop, not a presentation order. A grouping
    // column is cast to its derived ENUM above, and DuckDB orders an ENUM
    // by its declaration order — which `refresh_enum` builds from a bare
    // `select distinct`, so it is neither alphabetical nor stable across
    // an ingest that rebuilds the type. What holds is that two queries
    // against one generation return rows in the same order. A view that
    // wants a meaningful order should declare `sort`, which is emitted
    // ahead of these.
    let mut order_keys = vec!["s.row_depth asc".to_string()];
    for s in &view.sort {
        order_keys.push(format!(
            "\"{}\" {}",
            s.column,
            if s.descending { "desc" } else { "asc" }
        ));
    }
    for g in &view.grouping {
        let key = format!("\"{g}\" asc");
        if !order_keys.contains(&key) {
            order_keys.push(key);
        }
    }
    let order = format!(" order by {}", order_keys.join(", "));

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
        resolved_as_of,
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

    fn compile_with(store: &crate::store::Store, view: &ViewSpec) -> CompiledQuery {
        compile_view(
            store.writer(),
            view,
            &schema(),
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap()
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
    fn an_as_of_join_finds_a_reference_dataset_that_was_published_once() {
        // The reference dataset has one generation, and it is in live —
        // nothing has ever superseded it. Resolving the join's generations
        // from the archive alone found none, the join's predicate became
        // `false`, and every reference column in every as-of answer was
        // NULL, with no error.
        let (_d, store) = fixture();
        let schema = joined_schema();
        store
            .apply_schema(schema.dataset("instrument_ref").unwrap())
            .unwrap();
        store
            .writer()
            .execute_batch(
                "delete from risk_snapshot_underlying_live;
                 insert into risk_snapshot_underlying_archive values
                   ('BK0','L0','P1','C','I1','SPX', 10, 'b', 1, 1, TIMESTAMPTZ '2026-08-01 00:00:00Z');
                 insert into instrument_ref_instrument_live
                 values ('BK0','L0','P1','C','I1', 4200.0, 'b', 1, 1, TIMESTAMPTZ '2026-08-01 00:00:00Z');",
            )
            .unwrap();
        let at = chrono::DateTime::parse_from_rfc3339("2026-08-15T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let q = compile_view(
            store.writer(),
            &joined_view(),
            &schema,
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::At(at),
            usize::MAX,
        )
        .unwrap();
        let rows = run(&store, &q, &["row_depth", "instrument_ref", "strike"]);
        let i1 = rows
            .iter()
            .find(|r| r[1] == "Some(\"I1\")")
            .unwrap_or_else(|| panic!("{rows:?}"));
        assert_eq!(i1[2], "Some(4200.0)", "{rows:?}");
        assert_eq!(
            q.resolved_as_of
                .get("instrument_ref")
                .map(|t| t.to_rfc3339()),
            Some("2026-08-01T00:00:00+00:00".to_string()),
            "and the join's freshness is the generation it read"
        );
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

    /// Parse the `Some(n)` the `run` helper formats depths as. It reads
    /// every column as f64 first, so a depth arrives as "Some(1.0)".
    fn depths(rows: &[Vec<String>]) -> Vec<i64> {
        rows.iter()
            .map(|r| {
                r[0].trim_start_matches("Some(")
                    .trim_end_matches(')')
                    .parse::<f64>()
                    .unwrap_or_else(|_| panic!("unparsable row_depth {:?}", r[0]))
                    as i64
            })
            .collect()
    }

    #[test]
    fn rows_arrive_shallowest_first_when_the_view_declares_no_sort() {
        // The blotter flattens the tree by walking the rows in order and
        // assumes a parent is already placed when its children arrive.
        // Most views declare no sort, so this is the ordinary path, and
        // without an ORDER BY the order is whatever the plan happens to
        // produce. Widened past the two-row fixture, because a handful of
        // rows can come back depth-ordered by luck.
        let (_d, store) = fixture();
        let mut inserts = String::new();
        for i in 0..40 {
            inserts.push_str(&format!(
                "insert into risk_snapshot_position_live values
                   ('BK{i}','L{i}','P{i}','C', {i}, 'b', 1, 1, now());
                 insert into risk_snapshot_underlying_live values
                   ('BK{i}','L{i}','P{i}','C','I{i}','U{i}', {i}, 'b', 1, 1, now());",
            ));
        }
        store.writer().execute_batch(&inserts).unwrap();

        let q = compile(&store);
        assert!(
            view().sort.is_empty(),
            "this test is about the no-sort path; view() has grown a sort"
        );
        let rows = run(&store, &q, &["row_depth"]);
        let d = depths(&rows);
        assert!(
            d.len() > 40,
            "expected a tree of some size, got {}",
            d.len()
        );
        assert!(
            d.windows(2).all(|w| w[0] <= w[1]),
            "not shallowest-first, so a child can precede its parent: {d:?}"
        );
    }

    #[test]
    fn the_row_order_is_total_so_a_requery_does_not_reshuffle() {
        // A tile requeries on a timer. If the order is only "by depth",
        // rows within a depth may interleave differently each run and the
        // tree visibly reshuffles though nothing changed. Asserted as a
        // total order over the grouping columns rather than by running the
        // query repeatedly: identical input to one plan is not where the
        // nondeterminism would show, so a loop here would prove nothing.
        let (_d, store) = fixture();
        let q = compile(&store);
        let sql = q.sql.to_lowercase();
        let order = sql
            .rsplit_once(" order by ")
            .map(|(_, o)| o.to_string())
            .unwrap_or_else(|| panic!("no order by in:\n{}", q.sql));
        assert!(
            order.starts_with("s.row_depth asc"),
            "depth must lead the order: {order}"
        );
        for g in &view().grouping {
            assert!(
                order.contains(&format!("\"{g}\"")),
                "grouping column {g} is not a tie-breaker: {order}"
            );
        }
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

    /// A deliberately hostile store: a NULL book, a NULL LHU, two
    /// archived generations, a dimension value that exists only in
    /// history, and derived ENUMs actually built from live.
    ///
    /// Every one of those is ordinary in the real feed and none of them
    /// were in the original fixture, which is why three rounds of review
    /// each found defects the suite could not see. Reviews find what the
    /// fixture makes reachable.
    fn hostile_fixture() -> (tempfile::TempDir, crate::store::Store) {
        let (dir, store) = fixture();
        let conn = store.writer();
        conn.execute_batch(
            // Live: one NULL-book row and one NULL-LHU row alongside BK0.
            "insert into risk_snapshot_position_live values
               ('BK0',NULL,'P9','C', 5, 'b', 1, 1, now()),
               (NULL,'L0','P8','C', 11, 'b', 1, 1, now());
             insert into risk_snapshot_underlying_live values
               ('BK0',NULL,'P9','C','I9','SPX', 3, 'b', 1, 1, now()),
               (NULL,'L0','P8','C','I8','SPX', 6, 'b', 1, 1, now());
             -- Archive: two generations of one partition, plus a
             -- NULL-book partition, plus an LHU that has since retired.
             insert into risk_snapshot_position_archive values
               ('BK0','GONE','P1','C', 100, 'b', 1, 1,
                TIMESTAMPTZ '2026-08-01 00:00:00Z'),
               ('BK0','L0','P1','C', 200, 'b', 2, 2,
                TIMESTAMPTZ '2026-08-10 00:00:00Z'),
               (NULL,'L0','P8','C', 50, 'b', 3, 1,
                TIMESTAMPTZ '2026-08-01 00:00:00Z'),
               -- A partition with history at the position grain and none
               -- at the underlying grain: a cash-only book. Ordinary, and
               -- invisible to a generation set resolved from one grain.
               ('BK7','L7','P7','C', 900, 'cash', 4, 1,
                TIMESTAMPTZ '2026-08-01 00:00:00Z');
             insert into risk_snapshot_underlying_archive values
               ('BK0','GONE','P1','C','I1','SPX', 40, 'b', 1, 1,
                TIMESTAMPTZ '2026-08-01 00:00:00Z'),
               ('BK0','L0','P1','C','I1','SPX', 80, 'b', 2, 2,
                TIMESTAMPTZ '2026-08-10 00:00:00Z'),
               (NULL,'L0','P8','C','I8','SPX', 20, 'b', 3, 1,
                TIMESTAMPTZ '2026-08-01 00:00:00Z');",
        )
        .unwrap();
        // Build the ENUMs the way ingest does — from live only. Only for
        // columns this grain's table actually has; `underlying2_ref`
        // lives at the pair grain.
        for col in crate::store::ddl::dimension_columns(schema().dataset("risk_snapshot").unwrap())
            .into_iter()
            .filter(|c| Grain::Underlying.key_columns().contains(c))
        {
            crate::store::ddl::refresh_enum(
                conn,
                "risk_snapshot",
                col,
                &table_name("risk_snapshot", Grain::Underlying, TableKind::Live),
            )
            .unwrap();
        }
        (dir, store)
    }

    #[test]
    fn as_of_survives_a_value_that_has_since_left_live() {
        // The ENUMs are rebuilt from live on every ingest, so an archived
        // row holding a retired value cannot be cast through them. This
        // failed outright with a conversion error, not a wrong number.
        let (_d, store) = hostile_fixture();
        let at = chrono::DateTime::parse_from_rfc3339("2026-08-05T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let q = compile_view(
            store.writer(),
            &view(),
            &schema(),
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::At(at),
            usize::MAX,
        )
        .unwrap();
        let rows = run(&store, &q, &["row_depth", "lhu"]);
        assert!(
            rows.iter().any(|r| r[1] == "Some(\"GONE\")"),
            "the retired LHU must still be readable in history: {rows:?}"
        );
    }

    #[test]
    fn a_null_book_partition_is_not_dropped_from_history() {
        // Ingest permits rows with no book — it reports them rather than
        // dropping them — so a NULL-book partition is a real partition.
        // `book = '…'` cannot match it, so every as-of query silently
        // answered with less data than it had.
        let (_d, store) = hostile_fixture();
        let at = chrono::DateTime::parse_from_rfc3339("2026-08-05T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let q = compile_view(
            store.writer(),
            &view(),
            &schema(),
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::At(at),
            usize::MAX,
        )
        .unwrap();
        let rows = run(&store, &q, &["row_depth", "daily_trading_pnl"]);
        let total = rows.iter().find(|r| r[0] == "Some(0.0)").unwrap();
        assert_eq!(
            total[1], "Some(1050.0)",
            "100 from BK0, 50 from the NULL-book partition, 900 from the \
             cash-only one: {rows:?}"
        );
    }

    #[test]
    fn a_partition_with_history_at_only_one_grain_is_not_dropped() {
        // A generation is a file and a file publishes every grain, but a
        // partition can exist at one grain and not another — a cash-only
        // book has no underlying rows. Resolving the generation set from
        // the spine grain alone deleted those partitions from every
        // grain's answer, silently.
        let (_d, store) = hostile_fixture();
        let at = chrono::DateTime::parse_from_rfc3339("2026-08-05T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let q = compile_view(
            store.writer(),
            &view(),
            &schema(),
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::At(at),
            usize::MAX,
        )
        .unwrap();
        // The spine is the underlying grain, which has no rows for the
        // cash-only book — but its position-grain measure still belongs
        // in the total.
        let rows = run(&store, &q, &["row_depth", "daily_trading_pnl"]);
        let total = rows.iter().find(|r| r[0] == "Some(0.0)").unwrap();
        assert_eq!(
            total[1], "Some(1050.0)",
            "the cash-only book's 900 must be in the total: {rows:?}"
        );
    }

    #[test]
    fn an_as_of_semi_join_filters_the_probe_by_generation() {
        // The probe reads the archive, so without a generation predicate
        // it sees *every* archived generation at once — a semi-join that
        // matches on data the caller cannot see. The two archived
        // generations differ in `underlying_ref`'s neighbourhood, so a
        // probe that ignores generations admits the wrong one.
        let (_d, store) = hostile_fixture();
        store
            .writer()
            .execute_batch(
                // Generation 2 (2026-08-10) introduces RUT; generation 1
                // has only SPX. A query as of 08-05 must not see RUT.
                // Same key as the gen-1 position row ('GONE'), so the
                // only thing that can exclude it is the generation filter.
                "insert into risk_snapshot_underlying_archive values
                   ('BK0','GONE','P1','C','I1','RUT', 5, 'b', 2, 2,
                    TIMESTAMPTZ '2026-08-10 00:00:00Z');",
            )
            .unwrap();

        let scope = Scope {
            dimensions: vec![geode_core::scope::DimensionSelection {
                column: "underlying_ref".into(),
                values: vec!["RUT".into()],
            }],
            ..Scope::default()
        };
        let at = chrono::DateTime::parse_from_rfc3339("2026-08-05T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
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
        let rows = run(&store, &q, &["row_depth", "daily_trading_pnl"]);
        let total = rows.iter().find(|r| r[0] == "Some(0.0)").unwrap();
        assert_eq!(
            total[1], "None",
            "RUT did not exist yet at 08-05, so nothing qualifies: {rows:?}"
        );
    }

    #[test]
    fn a_measure_predicate_is_direct_not_semi_joined() {
        // "Not a key column at this grain" is not "finer". A measure
        // declared at this grain is on this very table, so filtering on
        // it is direct — badging it SemiJoined tells the trader
        // "positions that have…" about a filter that is nothing of the
        // kind, which inverts §6.3's attribution contract on screen.
        let (_d, store) = fixture();
        let ds = schema();
        let sql = crate::query::scope_sql::compile_scope(
            store.writer(),
            &Scope {
                expression: Some(geode_core::scope::parse_expr("daily_trading_pnl > 0.5").unwrap()),
                ..Scope::default()
            },
            ds.dataset("risk_snapshot").unwrap(),
            Grain::Position,
            &DerivedDimensions::default(),
            crate::query::scope_sql::Era::live(),
        )
        .unwrap();
        assert_eq!(
            sql.semantics,
            ScopeSemantics::Direct,
            "a measure at this grain is a direct filter: {}",
            sql.predicate
        );
        assert!(
            !sql.predicate.contains("exists"),
            "and needs no semi-join: {}",
            sql.predicate
        );
    }

    #[test]
    fn a_derived_dimension_predicate_is_direct_not_semi_joined() {
        // Same rule through the derived-dimension path: `desk` resolves
        // to `book`, which is a key column here, so it is direct.
        let (_d, store) = fixture();
        let ds = schema();
        let sql = crate::query::scope_sql::compile_scope(
            store.writer(),
            &Scope {
                expression: Some(geode_core::scope::parse_expr("desk = 'EU'").unwrap()),
                ..Scope::default()
            },
            ds.dataset("risk_snapshot").unwrap(),
            Grain::Position,
            &desks(),
            crate::query::scope_sql::Era::live(),
        )
        .unwrap();
        assert_eq!(sql.semantics, ScopeSemantics::Direct, "{}", sql.predicate);
        assert!(sql.predicate.contains("\"book\""), "{}", sql.predicate);
    }

    #[test]
    fn excluding_an_unmapped_derived_value_keeps_everything() {
        // `desk != 'NOWHERE'` excludes nothing, because no book maps to
        // it. The negated-empty branch must yield `true`, not `false` —
        // the difference between the whole desk and an empty screen.
        let (_d, store) = fixture();
        let ds = schema();
        let compile = |text: &str| {
            crate::query::scope_sql::compile_scope(
                store.writer(),
                &Scope {
                    expression: Some(geode_core::scope::parse_expr(text).unwrap()),
                    ..Scope::default()
                },
                ds.dataset("risk_snapshot").unwrap(),
                Grain::Position,
                &desks(),
                crate::query::scope_sql::Era::live(),
            )
            .unwrap()
            .predicate
        };
        assert!(
            compile("desk != 'NOWHERE'").contains("true"),
            "excluding nothing keeps everything: {}",
            compile("desk != 'NOWHERE'")
        );
        assert!(
            compile("desk = 'NOWHERE'").contains("false"),
            "selecting nothing keeps nothing: {}",
            compile("desk = 'NOWHERE'")
        );
    }

    #[test]
    fn a_finer_and_a_direct_predicate_bind_to_their_own_placeholders() {
        // Scoping by book *and* underlying is the most ordinary thing a
        // trader does. The finer predicate is emitted last, inside the
        // semi-join wrapper, while its value was bound first — so the two
        // values swapped, `book = 'SPX'` matched nothing, and the coarse
        // measure came back blank beside a correct fine-grained one.
        let (_d, store) = fixture();
        let scope = Scope {
            dimensions: vec![
                // Finer than position grain: routed into the semi-join.
                geode_core::scope::DimensionSelection {
                    column: "underlying_ref".into(),
                    values: vec!["SPX".into()],
                },
                // At position grain: stays direct, and is emitted first.
                geode_core::scope::DimensionSelection {
                    column: "book".into(),
                    values: vec!["BK0".into()],
                },
            ],
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
        let rows = run(&store, &q, &["row_depth", "delta01", "daily_trading_pnl"]);
        let total = rows.iter().find(|r| r[0] == "Some(0.0)").unwrap();
        assert_eq!(total[1], "Some(10.0)", "SPX's delta only: {rows:?}");
        assert_eq!(
            total[2], "Some(7.0)",
            "the position's PnL must survive the semi-join: {rows:?}"
        );
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

        // Live is read — the current generation lives nowhere else — but
        // only ever beside its archive, under the generation predicate.
        // A bare live reference anywhere is today's data leaking in.
        assert_eq!(
            q.sql.matches("_live").count(),
            q.sql.matches("union all select * from").count(),
            "every live reference must be one half of an era relation: {}",
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
    fn as_of_after_the_current_generation_reads_the_current_generation() {
        // The generation a partition holds now is in live and nowhere
        // else. Resolving from the archive alone answered "as of an hour
        // ago" with this morning's *previous* file (100, not 7), and found
        // nothing at all for BK1, which has only ever been published once.
        let (_d, store) = fixture();
        store
            .writer()
            .execute_batch(
                "delete from risk_snapshot_position_live;
                 delete from risk_snapshot_underlying_live;
                 insert into risk_snapshot_position_archive values
                   ('BK0','L0','P1','C', 100, 'b', 1, 1, TIMESTAMPTZ '2026-08-01 00:00:00Z');
                 insert into risk_snapshot_position_live values
                   ('BK0','L0','P1','C', 7, 'b', 2, 2, TIMESTAMPTZ '2026-08-10 00:00:00Z'),
                   ('BK1','L1','P2','C', 30, 'b1', 3, 3, TIMESTAMPTZ '2026-08-01 00:00:00Z');
                 insert into risk_snapshot_underlying_archive values
                   ('BK0','L0','P1','C','I1','SPX', 40, 'b', 1, 1, TIMESTAMPTZ '2026-08-01 00:00:00Z');
                 insert into risk_snapshot_underlying_live values
                   ('BK0','L0','P1','C','I1','SPX', 10, 'b', 2, 2, TIMESTAMPTZ '2026-08-10 00:00:00Z'),
                   ('BK1','L1','P2','C','I2','SPX', 5, 'b1', 3, 3, TIMESTAMPTZ '2026-08-01 00:00:00Z');",
            )
            .unwrap();
        let at = |s: &str| {
            chrono::DateTime::parse_from_rfc3339(s)
                .unwrap()
                .with_timezone(&chrono::Utc)
        };
        let totals = |t: &str| {
            let q = compile_view(
                store.writer(),
                &view(),
                &schema(),
                &Scope::default(),
                &DerivedDimensions::default(),
                &crate::query::as_of::AsOf::At(at(t)),
                usize::MAX,
            )
            .unwrap();
            let rows = run(&store, &q, &["row_depth", "delta01", "daily_trading_pnl"]);
            let total = rows.iter().find(|r| r[0] == "Some(0.0)").unwrap().clone();
            (total[1].clone(), total[2].clone(), q.resolved_as_of)
        };

        let (delta, pnl, resolved) = totals("2026-08-15T00:00:00Z");
        assert_eq!(pnl, "Some(37.0)", "BK0 at gen 2 (7) plus BK1 at gen 3 (30)");
        assert_eq!(
            delta, "Some(15.0)",
            "10 + 5, the generations current at 08-15"
        );
        // The label is the *oldest* partition read — §4.5's stalest-book
        // rule, the same one live freshness applies — not the newest.
        assert_eq!(
            resolved.get("risk_snapshot"),
            Some(&at("2026-08-01T00:00:00Z")),
            "BK1's 08-01 generation is the stalest input"
        );

        // Before BK0's second generation, the archived one is current and
        // live's must not leak in.
        let (delta, pnl, _) = totals("2026-08-05T00:00:00Z");
        assert_eq!(pnl, "Some(130.0)", "BK0 at gen 1 (100) plus BK1 (30)");
        assert_eq!(delta, "Some(45.0)", "40 + 5");
    }

    /// The real desk schema declares the pair grain, which makes it the
    /// finest declared grain — and its `underlying_ref` is `least(u1, u2)`.
    fn schema_with_pairs() -> SchemaSpec {
        let mut s = schema();
        let ds = s
            .datasets
            .iter_mut()
            .find(|d| d.name == "risk_snapshot")
            .unwrap();
        ds.columns.push(geode_core::schema::ColumnSpec {
            name: "cross_gamma02".into(),
            source_name: None,
            ty: geode_core::schema::ColumnType::F64,
            required: false,
            textual: false,
            role: ColumnRole::Measure {
                grain: Grain::UnderlyingPair,
                aggregate: Aggregate::Sum,
            },
        });
        s
    }

    /// One worst-of over RUT and SPX: two underlying rows and one
    /// canonical pair row `(RUT, SPX)`, in which SPX is `underlying2_ref`.
    fn pair_fixture() -> (tempfile::TempDir, crate::store::Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        store
            .apply_schema(schema_with_pairs().dataset("risk_snapshot").unwrap())
            .unwrap();
        store
            .writer()
            .execute_batch(
                "insert into risk_snapshot_position_live values
                   ('BK0','L0','P1','C', 7, 'b', 1, 1, now());
                 insert into risk_snapshot_underlying_live values
                   ('BK0','L0','P1','C','I1','SPX', 10, 'b', 1, 1, now()),
                   ('BK0','L0','P1','C','I1','RUT', 20, 'b', 1, 1, now());
                 insert into risk_snapshot_underlying_pair_live values
                   ('BK0','L0','P1','C','I1','RUT','SPX', 3, 'b', 1, 1, now());",
            )
            .unwrap();
        (dir, store)
    }

    #[test]
    fn scoping_to_an_underlying_that_sorts_second_in_its_pair_keeps_the_position() {
        // The single most common trader action, on the real schema. The
        // pair table's `underlying_ref` is `least(u1, u2)`, so a predicate
        // applied to it directly misses every pair where SPX sorts second
        // — every pair, on a worst-of over NDX/RUT/SPX. The spine and the
        // semi-join probe both did exactly that, and the tree came back
        // as a lone total row with a blank PnL.
        let (_d, store) = pair_fixture();
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
            &schema_with_pairs(),
            &scope,
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap();
        assert!(
            !q.sql
                .contains("from risk_snapshot_underlying_pair_live probe"),
            "the pair table must never be the probe for an underlying: {}",
            q.sql
        );
        let rows = run(
            &store,
            &q,
            &[
                "row_depth",
                "underlying_ref",
                "delta01",
                "daily_trading_pnl",
            ],
        );
        let total = rows.iter().find(|r| r[0] == "Some(0.0)").unwrap();
        assert_eq!(total[2], "Some(10.0)", "SPX's delta: {rows:?}");
        assert_eq!(
            total[3], "Some(7.0)",
            "the position has SPX risk, so its PnL survives: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|r| r[0] == "Some(2.0)" && r[1] == "Some(\"SPX\")"),
            "and SPX has its own row under the LHU: {rows:?}"
        );
    }

    #[test]
    fn the_underlying_level_shows_every_underlying_not_the_first_of_each_pair() {
        // Unscoped, the same fixture: a spine on the pair table showed RUT
        // (the pair's `least`) and never SPX, and SPX's delta joined to
        // nothing while still counting in the total.
        let (_d, store) = pair_fixture();
        let q = compile_view(
            store.writer(),
            &view(),
            &schema_with_pairs(),
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap();
        let rows = run(&store, &q, &["row_depth", "underlying_ref", "delta01"]);
        let mut level2: Vec<(String, String)> = rows
            .iter()
            .filter(|r| r[0] == "Some(2.0)")
            .map(|r| (r[1].clone(), r[2].clone()))
            .collect();
        level2.sort();
        assert_eq!(
            level2,
            vec![
                ("Some(\"RUT\")".to_string(), "Some(20.0)".to_string()),
                ("Some(\"SPX\")".to_string(), "Some(10.0)".to_string()),
            ],
            "{rows:?}"
        );
        let total = rows.iter().find(|r| r[0] == "Some(0.0)").unwrap();
        assert_eq!(total[2], "Some(30.0)", "and the children sum to it");
    }

    #[test]
    fn cross_gamma_is_blank_below_instrument_level_and_present_above_it() {
        // spec §6.3: the pair is canonical, so an underlying-level row has
        // no honest share of it; an instrument-level or coarser row does.
        let (_d, store) = pair_fixture();
        let mut v = view();
        v.columns.push(ViewColumn::Measure {
            name: "cross_gamma02".into(),
        });
        let q = compile_view(
            store.writer(),
            &v,
            &schema_with_pairs(),
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap();
        let rows = run(&store, &q, &["row_depth", "cross_gamma02"]);
        let at = |d: &str| -> Vec<String> {
            rows.iter()
                .filter(|r| r[0] == d)
                .map(|r| r[1].clone())
                .collect()
        };
        assert_eq!(at("Some(0.0)"), vec!["Some(3.0)"], "{rows:?}");
        assert_eq!(at("Some(1.0)"), vec!["Some(3.0)"], "{rows:?}");
        assert_eq!(at("Some(2.0)"), vec!["None", "None"], "{rows:?}");
        assert_eq!(at("Some(3.0)"), vec!["None", "None"], "{rows:?}");
        let cg = q
            .columns
            .iter()
            .find(|c| c.name == "cross_gamma02")
            .unwrap();
        assert_eq!(cg.attribution_by_depth[1], Attribution::Additive);
        assert_eq!(cg.attribution_by_depth[2], Attribution::NonAttributable);
    }

    #[test]
    fn a_blanked_cross_gamma_is_still_blank_after_the_snapshot_boundary() {
        // The companion to the test above, and the gap between them was
        // the defect. That one proves the *compiler* emits NULL, reading
        // through DuckDB's own row API. This one proves a module can still
        // tell, reading through `Snapshot` — which is the only way a
        // module ever sees a result. `f64_column` returns the raw Arrow
        // value buffer, so the cell §6.3 deliberately blanked arrived as a
        // confident 0.0: the rule was implemented in the compiler and
        // discarded one layer up, with every compiler test still green.
        use geode_core::snapshot::{ColumnMeta, Provenance, Snapshot};

        let (_d, store) = pair_fixture();
        let mut v = view();
        v.columns.push(ViewColumn::Measure {
            name: "cross_gamma02".into(),
        });
        let q = compile_view(
            store.writer(),
            &v,
            &schema_with_pairs(),
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap();

        let conn = store.writer();
        let mut stmt = conn.prepare(&q.sql).unwrap();
        let batches: Vec<_> = stmt
            .query_arrow(duckdb::params_from_iter(q.params.iter()))
            .unwrap()
            .collect();
        let meta: Vec<ColumnMeta> = q
            .columns
            .iter()
            .map(|c| ColumnMeta {
                name: c.name.clone(),
                attribution_by_depth: c.attribution_by_depth.clone(),
                scope_semantics: c.scope_semantics.clone(),
            })
            .collect();
        // DuckDB emits `row_depth` as Int32, not Int64. Pinned here
        // because the whole test turns on the snapshot being able to read
        // it: matching only Int64 made depth_of_row return None for every
        // real result while every Int64-based fixture passed.
        assert_eq!(
            format!(
                "{:?}",
                batches[0]
                    .schema()
                    .field_with_name("row_depth")
                    .unwrap()
                    .data_type()
            ),
            "Int32"
        );
        let snap =
            Snapshot::from_batches(batches, meta, v.grouping.len(), Provenance::default()).unwrap();

        let mut blanked = 0;
        for row in 0..snap.rows() {
            let depth = snap.depth_of_row(row).expect("every row carries a depth");
            match snap.f64_value("cross_gamma02", row) {
                None => {
                    assert!(
                        depth >= 2,
                        "row {row} at depth {depth} is blank but should carry the pair total"
                    );
                    blanked += 1;
                }
                Some(v) => {
                    assert!(
                        depth < 2,
                        "row {row} at depth {depth} reads {v} where §6.3 blanked it"
                    );
                    assert_eq!(v, 3.0, "row {row}");
                }
            }
        }
        assert!(
            blanked >= 4,
            "the fixture must actually contain blanked rows, or this asserts nothing: {blanked}"
        );
    }

    #[test]
    fn a_derived_column_inherits_the_attribution_of_what_it_references() {
        // A derived column was marked Additive/Direct unconditionally. An
        // expression over cross gamma therefore claimed to be summable at
        // a level where cross gamma itself is blanked (§6.3) — the marker
        // says "add this up" about a number built from one that must not
        // be. The marker is the only thing a renderer has to go on, so a
        // wrong one is worse than a missing column.
        let (_d, store) = pair_fixture();
        let mut v = view();
        v.columns.push(ViewColumn::Measure {
            name: "cross_gamma02".into(),
        });
        v.columns.push(ViewColumn::Derived {
            name: "cg_per_delta".into(),
            sql: "cross_gamma02 / nullif(delta01, 0)".into(),
        });
        let q = compile_view(
            store.writer(),
            &v,
            &schema_with_pairs(),
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap();

        let of = |name: &str| {
            q.columns
                .iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("no column {name}"))
                .clone()
        };
        let cross_gamma = of("cross_gamma02");
        let derived = of("cg_per_delta");

        assert_eq!(
            cross_gamma.attribution_by_depth[2],
            Attribution::NonAttributable,
            "precondition: cross gamma is blanked at depth 2"
        );
        assert_eq!(
            derived.attribution_by_depth[2],
            Attribution::NonAttributable,
            "an expression over a blanked measure is not additive"
        );
        assert_eq!(
            derived.attribution_by_depth[1], cross_gamma.attribution_by_depth[1],
            "and it tracks its inputs at every level, not just the worst"
        );
    }

    #[test]
    fn a_derived_column_over_one_measure_carries_exactly_that_measures_markers() {
        // The other half: the meet must be able to say "additive", or the
        // rule above is just "always blank". Over a single input the
        // answer is not a judgement call — it is that input's markers,
        // level for level, including the levels where they are additive.
        let (_d, store) = fixture();
        let mut v = view();
        v.columns.push(ViewColumn::Derived {
            name: "delta_doubled".into(),
            sql: "delta01 * 2".into(),
        });
        let q = compile_with(&store, &v);
        let of = |name: &str| {
            q.columns
                .iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("no column {name}"))
        };
        assert_eq!(
            of("delta_doubled").attribution_by_depth,
            of("delta01").attribution_by_depth,
        );
        assert!(
            of("delta01")
                .attribution_by_depth
                .contains(&Attribution::Additive),
            "precondition: this input is additive somewhere, or the \
             assertion above is satisfied by blanking everything"
        );
    }

    #[test]
    fn a_derived_column_referencing_nothing_selected_does_not_overclaim() {
        // A constant has no inputs to inherit from. It must not be given a
        // weaker marker than it has earned, nor a stronger one.
        let (_d, store) = fixture();
        let mut v = view();
        v.columns.push(ViewColumn::Derived {
            name: "one".into(),
            sql: "1".into(),
        });
        let q = compile_with(&store, &v);
        let derived = q.columns.iter().find(|c| c.name == "one").unwrap();
        assert!(
            derived
                .attribution_by_depth
                .iter()
                .all(|a| *a == Attribution::Additive),
            "{:?}",
            derived.attribution_by_depth
        );
    }

    #[test]
    fn a_column_name_inside_a_string_literal_is_not_a_reference() {
        // A label that happens to mention a column must not inherit that
        // column's markers.
        //
        // The literal needs a separator after the name. Without one the
        // closing quote's `current.clear()` discards it anyway, so a
        // single-token literal passes whether or not the `in_string` guard
        // is there — the first version of this test proved nothing, and
        // the mutation harness is what said so.
        let (_d, store) = fixture();
        let mut v = view();
        v.columns.push(ViewColumn::Derived {
            name: "label".into(),
            sql: "'daily_trading_pnl per unit'".into(),
        });
        let q = compile_with(&store, &v);
        let derived = q.columns.iter().find(|c| c.name == "label").unwrap();
        assert!(
            derived
                .attribution_by_depth
                .iter()
                .all(|a| *a == Attribution::Additive),
            "a name inside a literal is not a reference: {:?}",
            derived.attribution_by_depth
        );
    }

    #[test]
    fn a_stale_enum_degrades_that_column_instead_of_failing_the_query() {
        // The ENUM is refreshed at ingest (§3.6), so a value it does not
        // carry means something already went wrong upstream. A plain cast
        // makes that anomaly fail the *whole* statement, so one unknown
        // book costs the trader every row of the tile.
        let (_d, store) = fixture();
        store
            .writer()
            .execute_batch(
                "drop type if exists risk_snapshot_lhu_enum;
                 create type risk_snapshot_lhu_enum as enum ('L0');
                 insert into risk_snapshot_position_live values
                   ('BK9','L_UNKNOWN','P9','C', 5, 'b', 1, 1, now());
                 insert into risk_snapshot_underlying_live values
                   ('BK9','L_UNKNOWN','P9','C','I9','SPX', 3, 'b', 1, 1, now());",
            )
            .unwrap();

        let q = compile(&store);
        let rows = run(&store, &q, &["row_depth", "lhu"]);
        assert!(!rows.is_empty(), "the query must still answer: {}", q.sql);
        // The known value still reads; the unknown one degrades to blank
        // rather than taking the statement down with it.
        let lhus: Vec<&str> = rows.iter().map(|r| r[1].as_str()).collect();
        assert!(
            lhus.iter().any(|v| v.contains("L0")),
            "the known value must survive: {lhus:?}"
        );
    }

    #[test]
    fn a_measure_predicate_is_evaluated_at_the_measures_own_grain() {
        // `delta01 > 5` from position grain: the probe has to be the
        // underlying table, where the column is. Probing the spine's
        // grain instead was a binder error the moment the spine was a
        // grain that lacked the column — every grain but one.
        let (_d, store) = pair_fixture();
        let scope = Scope {
            expression: Some(geode_core::scope::parse_expr("delta01 > 15").unwrap()),
            ..Scope::default()
        };
        let q = compile_view(
            store.writer(),
            &view(),
            &schema_with_pairs(),
            &scope,
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap();
        let rows = run(&store, &q, &["row_depth", "delta01", "daily_trading_pnl"]);
        let total = rows.iter().find(|r| r[0] == "Some(0.0)").unwrap();
        assert_eq!(total[1], "Some(20.0)", "only RUT's delta passes: {rows:?}");
        assert_eq!(
            total[2], "Some(7.0)",
            "the position has a row that passes, so it is in: {rows:?}"
        );
        let pnl = q
            .columns
            .iter()
            .find(|c| c.name == "daily_trading_pnl")
            .unwrap();
        assert_eq!(
            pnl.scope_semantics,
            ScopeSemantics::SemiJoined {
                dimensions: vec!["delta01".into()]
            }
        );
    }

    #[test]
    fn a_pair_measure_predicate_reaches_both_underlyings_of_the_pair() {
        // `cross_gamma02 > 1` from underlying grain probes the pair table.
        // The keys the two grains share are the *dimension* keys — up to
        // `instrument_ref` — because the pair table's `underlying_ref` is
        // `least(u1, u2)`. Joining on it too would admit RUT (the pair's
        // `least`) and drop SPX from the same instrument.
        let (_d, store) = pair_fixture();
        let scope = Scope {
            expression: Some(geode_core::scope::parse_expr("cross_gamma02 > 1").unwrap()),
            ..Scope::default()
        };
        let q = compile_view(
            store.writer(),
            &view(),
            &schema_with_pairs(),
            &scope,
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap();
        let rows = run(&store, &q, &["row_depth", "delta01"]);
        let total = rows.iter().find(|r| r[0] == "Some(0.0)").unwrap();
        assert_eq!(
            total[1], "Some(30.0)",
            "both underlyings of the instrument that has the pair: {rows:?}"
        );
    }

    #[test]
    fn a_cash_only_position_has_a_row_at_the_lhu_level() {
        // A position with trading PnL and no greeks — a cash line, a fee.
        // The spine was scanned from the finest table, which has no row
        // for it, so its 900 was in the grand total and on no row beneath
        // it: the children did not sum to their parent.
        let (_d, store) = fixture();
        store
            .writer()
            .execute_batch(
                "insert into risk_snapshot_position_live values
                   ('BK0','L7','P7','C', 900, 'b', 1, 1, now());",
            )
            .unwrap();
        let q = compile(&store);
        let rows = run(&store, &q, &["row_depth", "lhu", "daily_trading_pnl"]);
        let total = rows.iter().find(|r| r[0] == "Some(0.0)").unwrap();
        assert_eq!(total[2], "Some(907.0)", "{rows:?}");
        let l7 = rows
            .iter()
            .find(|r| r[0] == "Some(1.0)" && r[1] == "Some(\"L7\")")
            .unwrap_or_else(|| panic!("the cash-only LHU needs a row: {rows:?}"));
        assert_eq!(l7[2], "Some(900.0)");
        // And nothing beneath it: the position has no underlying to sit
        // under, and inventing one would be an allocation.
        assert!(
            !rows
                .iter()
                .any(|r| r[1] == "Some(\"L7\")" && r[0] != "Some(1.0)"),
            "{rows:?}"
        );
    }

    #[test]
    fn a_scope_selecting_nothing_still_yields_the_grand_total_row() {
        // The tree always has its root. With the spine assembled from the
        // aggregates, an empty result would otherwise have no rows at all.
        let (_d, store) = fixture();
        let scope = Scope {
            dimensions: vec![geode_core::scope::DimensionSelection {
                column: "book".into(),
                values: vec!["NOWHERE".into()],
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
        let rows = run(&store, &q, &["row_depth", "delta01"]);
        assert_eq!(
            rows,
            vec![vec!["Some(0.0)".to_string(), "None".to_string()]]
        );
    }

    #[test]
    fn a_view_with_no_measures_still_has_every_level() {
        // Nothing to assemble the spine from, so it is scanned from the
        // finest grain carrying the grouping — the shape the whole spine
        // once had.
        let (_d, store) = fixture();
        let mut v = view();
        v.columns.clear();
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
        let rows = run(&store, &q, &["row_depth", "underlying_ref"]);
        assert_eq!(rows.len(), 6, "{rows:?}");
        assert!(
            rows.iter()
                .any(|r| r[0] == "Some(2.0)" && r[1] == "Some(\"RUT\")"),
            "{rows:?}"
        );
    }

    #[test]
    fn a_text_filter_reaches_coarse_measures_by_membership() {
        // `underlying_ref` is textual and finer than position grain. The
        // filter applied to the greeks and silently not to the PnL beside
        // them — and marked nothing (spec §6.3).
        let (_d, store) = fixture();
        let mut s = schema();
        s.datasets[0]
            .columns
            .iter_mut()
            .find(|c| c.name == "underlying_ref")
            .unwrap()
            .textual = true;
        store
            .writer()
            .execute_batch(
                "insert into risk_snapshot_position_live values
                   ('BK0','L0','P2','C', 3, 'b', 1, 1, now());
                 insert into risk_snapshot_underlying_live values
                   ('BK0','L0','P2','C','I2','NDX', 50, 'b', 1, 1, now());",
            )
            .unwrap();
        let scope = Scope {
            text: Some("SPX".into()),
            ..Scope::default()
        };
        let q = compile_view(
            store.writer(),
            &view(),
            &s,
            &scope,
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap();
        let rows = run(&store, &q, &["row_depth", "delta01", "daily_trading_pnl"]);
        let total = rows.iter().find(|r| r[0] == "Some(0.0)").unwrap();
        assert_eq!(total[1], "Some(10.0)", "{rows:?}");
        assert_eq!(
            total[2], "Some(7.0)",
            "positions that have SPX risk, not every position: {rows:?}"
        );
        let pnl = q
            .columns
            .iter()
            .find(|c| c.name == "daily_trading_pnl")
            .unwrap();
        assert_eq!(
            pnl.scope_semantics,
            ScopeSemantics::SemiJoined {
                dimensions: vec!["underlying_ref".into()]
            }
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
