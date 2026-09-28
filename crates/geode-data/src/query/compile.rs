//! Compile a view's rollup levels through the requested maximum depth into
//! one statement. Aggregate each measure at its own grain, then join at group
//! cardinality to prevent coarser measures from multiplying across finer rows.
//! Ungrouped dimensions use unanimity aggregates from a grain carrying the
//! entire grouping; these add display columns without changing tree rows.
//!
//! The caller can request one level beyond the expanded tree so a single-step
//! expansion uses the existing snapshot. Deeper levels require a new query.

use crate::query::scope_sql::{DictionaryCache, Era, compile_scope_cached};
use crate::store::StoreError;
use crate::store::ddl::TableKind;
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
    /// `None` for grouping columns, the depth marker, and mixed flags.
    pub grain: Option<Grain>,
    /// Indexed by depth, `0..=grouping.len()`.
    pub attribution_by_depth: Vec<Attribution>,
    pub scope_semantics: ScopeSemantics,
    /// Whether the column's values add up across sibling rows: only a
    /// plain measure whose schema aggregate is `sum`. A min/max/any
    /// measure, a derived expression (a ratio of sums is not a sum), a
    /// joined attribute, and any dimension are not — a footer that
    /// totalled them would print a plausible wrong number.
    pub summable: bool,
    /// For an ungrouped dimension column, the index in `columns` (and the
    /// result) of its boolean companion that is true where the rows under a
    /// tree row disagree. Carried unchanged to `ColumnMeta::mixed_flag`.
    pub mixed_flag: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct CompiledQuery {
    pub sql: String,
    pub params: Vec<Value>,
    pub grouping: Vec<String>,
    pub columns: Vec<CompiledColumn>,
    /// Datasets actually read, used to report the joined view's stalest input.
    pub stalest_input: Vec<String>,
    /// Oldest selected source time per dataset for a historical query. Report
    /// the data's age, not the requested instant: equal requested timestamps
    /// can conceal inputs whose actual generations differ by weeks.
    pub resolved_as_of: std::collections::BTreeMap<String, chrono::DateTime<chrono::Utc>>,
    /// Selected generation for a historical document request, or `None` if
    /// no generation matches. Historical views leave this unset because each
    /// partition resolves independently. Live queries obtain their generation
    /// marker from the catalog during execution in `query::read`.
    pub resolved_generation: Option<i64>,
}

fn quoted(cols: &[String]) -> Vec<String> {
    cols.iter().map(|c| format!("\"{c}\"")).collect()
}

/// Whether `grain` carries every requested dimension. Resolve derived names
/// to their source columns and use `DatasetSpec::carries` for both key and
/// carried dimensions, including inheritance to finer grains.
fn carries_all(
    ds: &DatasetSpec,
    grain: Grain,
    columns: &[String],
    dims: &DerivedDimensions,
) -> bool {
    columns
        .iter()
        .all(|col| ds.carries(grain, dims.base_column(col)))
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
        .find(|g| carries_all(ds, *g, columns, dims))
}

/// Find compiled columns referenced by a derived SQL expression.
///
/// Identifier tokens are matched against columns already produced by the view.
/// Token boundaries distinguish `delta01` from `delta01_usd`; quoted strings
/// and SQL comments cannot introduce references.
///
/// Uncertain tokens conservatively match more columns. Attribution is combined
/// by a meet, so a false positive can weaken a marker and blank an otherwise
/// valid cell. A missed reference could incorrectly claim additive or direct
/// attribution. Unbalanced quoting therefore falls back to all prior columns.
/// Comments are stripped first so apostrophes in prose cannot hide later names.
fn referenced_columns<'a>(sql: &str, columns: &'a [CompiledColumn]) -> Vec<&'a CompiledColumn> {
    let stripped = strip_sql_comments(sql);

    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    for ch in stripped.chars() {
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

    // Unbalanced quoting means the scan lost its place and the token list
    // cannot be trusted. Fail toward the weakest claim — every column —
    // rather than toward the strongest.
    if in_string {
        return columns.iter().collect();
    }

    columns
        .iter()
        .filter(|c| tokens.contains(&c.name))
        .collect()
}

/// Remove `-- …` line comments and `/* … */` block comments.
///
/// Only the identifier scan needs this; the expression itself goes to
/// DuckDB verbatim, comments and all.
fn strip_sql_comments(sql: &str) -> String {
    let bytes: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let ch = bytes[i];
        if in_string {
            out.push(ch);
            if ch == '\'' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if ch == '\'' {
            in_string = true;
            out.push(ch);
            i += 1;
        } else if ch == '-' && bytes.get(i + 1) == Some(&'-') {
            while i < bytes.len() && bytes[i] != '\n' {
                i += 1;
            }
        } else if ch == '/' && bytes.get(i + 1) == Some(&'*') {
            i += 2;
            while i < bytes.len() && !(bytes[i] == '*' && bytes.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
        } else {
            out.push(ch);
            i += 1;
        }
    }
    out
}

/// Single-quote escaping for a literal inlined into SQL. Derived
/// dimension values come from config, which is trusted but not
/// necessarily quote-free.
fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Project a derived dimension as a scalar `CASE` over its source column.
/// This preserves row cardinality even if mapping keys repeat, and unmapped
/// values become NULL. A lookup join could multiply rows or match rolled-up
/// NULL keys.
fn derived_expr(d: &geode_core::dimensions::DerivedDimension) -> String {
    format!("{} as \"{}\"", derived_case(d), d.name)
}

/// Unaliased derived-dimension `CASE`, shared with distinct-value queries
/// that name the output `value` instead of the dimension's name.
pub(crate) fn derived_case(d: &geode_core::dimensions::DerivedDimension) -> String {
    if d.values.is_empty() {
        return "NULL::varchar".to_string();
    }
    let arms: Vec<String> = d
        .values
        .iter()
        .map(|(source, derived)| {
            format!("when {} then {}", sql_literal(source), sql_literal(derived))
        })
        .collect();
    format!(
        "case \"{from}\" {arms} end",
        from = d.from,
        arms = arms.join(" ")
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
    ds: &DatasetSpec,
    columns: &[String],
    dims: &'a DerivedDimensions,
    grain: Grain,
) -> Vec<&'a geode_core::dimensions::DerivedDimension> {
    columns
        .iter()
        .filter_map(|c| dims.get(c))
        .filter(|d| ds.carries(grain, &d.from))
        .collect()
}

/// The table kind, optional generation predicate, and oldest selected source
/// time for one dataset. Shared by view and distinct-value compilation.
pub(crate) struct ResolvedEra {
    pub kind: TableKind,
    pub generations: Option<String>,
    pub resolved_as_of: std::collections::BTreeMap<String, chrono::DateTime<chrono::Utc>>,
}

impl ResolvedEra {
    pub(crate) fn era(&self) -> Era<'_> {
        Era {
            kind: self.kind,
            generations: self.generations.as_deref(),
        }
    }
}

/// Resolve a dataset's read era. Live uses its table without a generation
/// predicate. Historical reads resolve the summary covering all live/archive
/// pairs, including partitions represented at only one grain.
pub(crate) fn era_for(
    conn: &Connection,
    dataset: &str,
    as_of: &crate::query::as_of::AsOf,
) -> Result<ResolvedEra, StoreError> {
    let mut resolved_as_of: std::collections::BTreeMap<String, chrono::DateTime<chrono::Utc>> =
        std::collections::BTreeMap::new();
    let (kind, generations) = match as_of {
        crate::query::as_of::AsOf::Live => (TableKind::Live, None),
        crate::query::as_of::AsOf::At(t) => {
            let gens = crate::query::as_of::resolve_generations(conn, dataset, *t)?;
            if let Some(oldest) = gens.iter().map(|g| g.source_time).min() {
                resolved_as_of.insert(dataset.to_string(), oldest);
            }
            (
                TableKind::Archive,
                Some(crate::query::as_of::generation_predicate(&gens)),
            )
        }
    };
    Ok(ResolvedEra {
        kind,
        generations,
        resolved_as_of,
    })
}

/// The result column carrying an ungrouped dimension's mixed flag. Linked to
/// its value column by index (`CompiledColumn::mixed_flag`), not by this
/// name; the name only has to be distinct, which the compiler checks.
fn mixed_flag_name(column: &str) -> String {
    format!("{column}#mixed")
}

/// The two aggregates the unanimity rule needs for one ungrouped dimension,
/// named for the column and its flag.
///
/// The value is kept only when every row has one and they are all equal:
/// `count(c) = count(*)` rules out a NULL among values, `min = max` rules
/// out two different values. The flag is set when there is some value but
/// the rows are not unanimous — a NULL beside a value is mixed, because
/// showing the value would claim it for the rows that have none. No value at
/// all (no rows, or every row NULL) is neither: blank. Never `any_value`: a
/// position holds several instruments, and an arbitrary leg's strike is a
/// plausible wrong value. `min`/`max`/`count` rather than
/// `count(distinct)` keeps it one cheap pass.
fn unanimity_aggregates(column: &str) -> [String; 2] {
    let c = format!("\"{column}\"");
    [
        format!("case when count({c}) = count(*) and min({c}) = max({c}) then min({c}) end as {c}"),
        format!(
            "(count({c}) > 0 and (count({c}) < count(*) or min({c}) <> max({c}))) as \"{}\"",
            mixed_flag_name(column)
        ),
    ]
}

/// How one grain's aggregate lines up with the spine: which grouping columns
/// it carries within the materialized depth, its grouping sets and level
/// marker, and the join condition that attaches each of its levels to the
/// spine rows of the matching depth. Shared by measure aggregates and the
/// unanimity-only aggregate, so the two cannot attach differently.
struct GrainShape {
    /// Only the grouping columns this grain carries, and only within the
    /// materialized depth — selecting a key the spine no longer groups by
    /// would leave it outside every aggregate.
    own: Vec<String>,
    /// How many of *this grain's* grouping columns are present at each spine
    /// depth. The aggregate groups by the same prefixes projected onto the
    /// columns it has, so this is the map between the spine's depth and the
    /// aggregate's own level.
    own_present: Vec<usize>,
    sub_group: String,
    sub_depth: String,
    on: String,
}

impl GrainShape {
    fn new(
        ds: &DatasetSpec,
        dims: &DerivedDimensions,
        grain: Grain,
        grouping: &[String],
        depth: usize,
        alias: &str,
    ) -> GrainShape {
        let own: Vec<String> = grouping[..depth]
            .iter()
            .filter(|g| ds.carries(grain, dims.base_column(g)))
            .cloned()
            .collect();
        let own_q = quoted(&own);

        let own_present: Vec<usize> = (0..=depth)
            .map(|d| own.iter().filter(|c| grouping[..d].contains(c)).count())
            .collect();

        // The same depths, projected onto the columns this grain has.
        let sub_group = if own.is_empty() {
            String::new()
        } else {
            let mut sets: Vec<String> = (0..=depth)
                .map(|d| {
                    let kept: Vec<String> = own
                        .iter()
                        .filter(|c| grouping[..d].contains(c))
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
        GrainShape {
            own,
            own_present,
            sub_group,
            sub_depth,
            on,
        }
    }

    /// The aggregate CTE over `relation`, grouped to this shape's levels.
    fn cte(&self, alias: &str, aggs: &[String], relation: &str, pred: &str) -> String {
        format!(
            "{alias} as (select {keys}{comma}{sub_depth}, {aggs} \
             from {relation} base where {pred}{sub_group})",
            keys = quoted(&self.own).join(", "),
            comma = if self.own.is_empty() { "" } else { ", " },
            sub_depth = self.sub_depth,
            aggs = aggs.join(", "),
            sub_group = self.sub_group,
        )
    }
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
    compile_view_with_cache(
        conn,
        view,
        schema,
        scope,
        dims,
        as_of,
        max_depth,
        &mut DictionaryCache::default(),
    )
}

/// Compile with a caller-supplied dictionary cache. Dataset ENUM types and
/// pattern matches are reused across measure grains, spine queries and the
/// interned-column check. Tests supply a cache to verify lookup counts;
/// [`compile_view`] creates a fresh cache for each statement.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compile_view_with_cache(
    conn: &Connection,
    view: &ViewSpec,
    schema: &SchemaSpec,
    scope: &Scope,
    dims: &DerivedDimensions,
    as_of: &crate::query::as_of::AsOf,
    max_depth: usize,
    cache: &mut DictionaryCache,
) -> Result<CompiledQuery, StoreError> {
    let ds = schema
        .dataset(&view.dataset)
        .ok_or_else(|| compile_error(view, format!("unknown dataset '{}'", view.dataset)))?;
    if ds.computed {
        return Err(compile_error(
            view,
            format!(
                "dataset '{}' is computed by a module and has no tables",
                ds.name
            ),
        ));
    }

    let n = view.grouping.len();
    let depth = max_depth.min(n);
    let group_cols = quoted(&view.grouping);
    let materialized = &view.grouping[..depth];
    let mut params: Vec<Value> = Vec::new();
    let mut ctes: Vec<String> = Vec::new();
    let mut selects: Vec<String> = Vec::new();
    let mut joins: Vec<String> = Vec::new();
    let mut columns: Vec<CompiledColumn> = Vec::new();

    // Live and historical reads share the statement shape. Historical
    // relations filter both archive and live by generations resolved across
    // all grains, so a partition absent from one grain remains visible
    // at the grains that contain it.
    let resolved = era_for(conn, &view.dataset, as_of)?;
    let mut resolved_as_of = resolved.resolved_as_of.clone();
    // One era for the whole statement: every aggregate, the spine's
    // fallback scan, the scope's membership probes and the cross-dataset
    // joins must all read the same relations.
    let era = resolved.era();

    // The spine contains the tree's `(grouping tuple, depth)` rows. Union
    // those rows from every grain's aggregates so entities present only at a
    // coarse grain, such as cash positions without underlyings, remain visible.
    // A constant grand-total row preserves the root even for an empty scope.
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

    // Ungrouped dimension columns, each read from the grain validation chose
    // for it (the coarsest carrying it alongside the whole grouping, so one
    // grain serves every depth). One validation cannot honour is a compile
    // error if required — reaching here with one means the caller skipped the
    // gate — and dropped if optional, as validation said it would be.
    let mut unanimous: Vec<(String, Grain)> = Vec::new();
    for u in view.ungrouped_dimensions(schema, dims) {
        match u.grain {
            Some(grain) => unanimous.push((u.name.to_string(), grain)),
            None if u.required => {
                return Err(compile_error(
                    view,
                    format!(
                        "column '{}' is an ungrouped dimension no declared grain of '{}' \
                         carries alongside the grouping {:?}",
                        u.name, view.dataset, view.grouping
                    ),
                ));
            }
            None => {}
        }
    }
    // The companion flag is a result column of its own; a view column that
    // already bears its name would make the snapshot's by-name lookups
    // ambiguous.
    for (name, _) in &unanimous {
        let flag = mixed_flag_name(name);
        if view.columns.iter().any(|c| c.name() == flag) || view.grouping.contains(&flag) {
            return Err(compile_error(
                view,
                format!("column '{flag}' collides with the mixed flag of '{name}'"),
            ));
        }
    }
    // Per grain: the alias whose CTE carries its unanimity aggregates, and the
    // grain's scope semantics, filled as each CTE is emitted.
    let mut unanimity_at: Vec<(Grain, String, ScopeSemantics)> = Vec::new();

    // One aggregate subquery per measure grain the view touches.
    for grain in view.measure_grains(schema) {
        let alias = format!("agg_{}", grain.table());
        let grain_scope = compile_scope_cached(conn, scope, ds, grain, dims, era, cache)?;

        let shape = GrainShape::new(ds, dims, grain, &view.grouping, depth, &alias);

        let measures: Vec<(&ColumnSpec, Aggregate)> = view
            .columns
            .iter()
            .filter_map(|c| match c {
                ViewColumn::Measure { name, .. } => ds.column(name),
                _ => None,
            })
            .filter_map(|c| match c.role {
                // Only a measure role supplies an aggregate. An attribute can
                // repeat across rows of its grain and must not be summed by
                // default, even when an optional declaration passes validation.
                ColumnRole::Measure { aggregate, .. } => Some((c, aggregate)),
                _ => None,
            })
            .filter(|(c, _)| c.grain() == Some(grain))
            .collect();
        if measures.is_empty() {
            continue;
        }

        let mut aggs: Vec<String> = measures
            .iter()
            .map(|(m, agg)| format!("{} as \"{}\"", agg.sql(&format!("\"{}\"", m.name)), m.name))
            .collect();
        // An ungrouped dimension read at this grain rides the scan the
        // measures already make, rather than a second one over the same table.
        let folded: Vec<&str> = unanimous
            .iter()
            .filter(|(_, g)| *g == grain)
            .map(|(name, _)| name.as_str())
            .collect();
        if !folded.is_empty() {
            aggs.extend(folded.iter().flat_map(|name| unanimity_aggregates(name)));
            unanimity_at.push((grain, alias.clone(), grain_scope.semantics.clone()));
        }

        ctes.push(shape.cte(
            &alias,
            &aggs,
            &scan(
                &era.relation(&view.dataset, grain),
                &derived_for(ds, &shape.own, dims, grain),
            ),
            &grain_scope.predicate,
        ));
        // The grain subquery's params follow the previous CTE's, in CTE
        // order.
        params.extend(grain_scope.params);

        // The depths this grain carries in full are spine levels it can
        // supply: at such a depth its own level *is* the spine's.
        let carried: Vec<usize> = (1..=depth)
            .filter(|d| shape.own_present[*d] == *d)
            .collect();
        if !carried.is_empty() {
            let projection: Vec<String> = materialized
                .iter()
                .map(|g| {
                    if shape.own.contains(g) {
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

        agg_joins.push(format!("left join {alias} on {}", shape.on));

        for (m, _) in measures {
            // Attribution per depth, from the schema alone.
            let by_depth: Vec<Attribution> = (0..=n)
                .map(|d| attribution_of(ds, grain, &view.grouping[..d], dims))
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
                summable: matches!(
                    m.role,
                    ColumnRole::Measure {
                        aggregate: Aggregate::Sum,
                        ..
                    }
                ),
                mixed_flag: None,
            });
        }
    }

    // A grain no measure aggregate reads gets a CTE of its own, holding only
    // the unanimity aggregates. It is joined to the spine but never feeds it:
    // adding a display column must not add or remove tree rows.
    let mut own_grains: Vec<Grain> = unanimous
        .iter()
        .map(|(_, g)| *g)
        .filter(|g| !unanimity_at.iter().any(|(at, _, _)| at == g))
        .collect();
    own_grains.sort_unstable();
    own_grains.dedup();
    for grain in own_grains {
        let alias = format!("dim_{}", grain.table());
        let grain_scope = compile_scope_cached(conn, scope, ds, grain, dims, era, cache)?;
        let shape = GrainShape::new(ds, dims, grain, &view.grouping, depth, &alias);
        let aggs: Vec<String> = unanimous
            .iter()
            .filter(|(_, g)| *g == grain)
            .flat_map(|(name, _)| unanimity_aggregates(name))
            .collect();
        ctes.push(shape.cte(
            &alias,
            &aggs,
            &scan(
                &era.relation(&view.dataset, grain),
                &derived_for(ds, &shape.own, dims, grain),
            ),
            &grain_scope.predicate,
        ));
        params.extend(grain_scope.params);
        agg_joins.push(format!("left join {alias} on {}", shape.on));
        unanimity_at.push((grain, alias, grain_scope.semantics));
    }

    // The outer select's unanimity columns, in view order: the value, in the
    // column's own type so a numeric dimension sorts as a number, then its
    // flag. With no matching grain rows, the value is NULL and the flag
    // is coalesced to false: the cell is blank.
    let mut unanimity_selects: Vec<String> = Vec::new();
    let mut unanimity_columns: Vec<CompiledColumn> = Vec::new();
    for (name, grain) in &unanimous {
        let Some((_, alias, semantics)) = unanimity_at.iter().find(|(g, _, _)| g == grain) else {
            continue;
        };
        let flag = mixed_flag_name(name);
        unanimity_selects.push(format!("{alias}.\"{name}\" as \"{name}\""));
        unanimity_selects.push(format!("coalesce({alias}.\"{flag}\", false) as \"{flag}\""));
        unanimity_columns.push(CompiledColumn {
            name: name.clone(),
            grain: Some(*grain),
            // The unanimity rule is exact at every depth: a value shown
            // is the value of every row beneath.
            attribution_by_depth: vec![Attribution::Additive; n + 1],
            scope_semantics: semantics.clone(),
            summable: false,
            // Filled with an absolute index once the column's position in
            // the result is known.
            mixed_flag: None,
        });
        unanimity_columns.push(CompiledColumn {
            name: flag,
            grain: None,
            attribution_by_depth: vec![Attribution::Additive; n + 1],
            scope_semantics: ScopeSemantics::Direct,
            summable: false,
            mixed_flag: None,
        });
    }

    // For depths no measure grain carries, scan the finest declared grain
    // that carries the grouping columns. This also covers views with no measures.
    let missing: Vec<usize> = (1..=depth).filter(|d| !covered[*d]).collect();
    if !missing.is_empty() {
        let spine_grain = finest_carrying(ds, materialized, dims).ok_or_else(|| {
            compile_error(
                view,
                format!(
                    "no declared grain of '{}' carries every grouping column {:?}; \
                     a grouping column must be carried as a dimension by some grain",
                    view.dataset, materialized
                ),
            )
        })?;
        let spine_scope = compile_scope_cached(conn, scope, ds, spine_grain, dims, era, cache)?;
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
                &derived_for(ds, materialized, dims, spine_grain),
            ),
            pred = &spine_scope.predicate,
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
        let existing = cache.enum_types(conn, &view.dataset)?.to_vec();
        crate::store::ddl::categorical_columns(ds)
            .into_iter()
            .filter(|c| existing.contains(&crate::store::ddl::enum_type_name(&view.dataset, c)))
            .collect()
    };
    for (i, g) in view.grouping.iter().enumerate() {
        // Cast dimensions to their derived ENUMs for dictionary-encoded output
        // and integer-code comparisons. Below the materialized depth, select
        // NULL for absent spine columns so snapshot shape stays unchanged.
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
            // Use `try_cast` so an incomplete ENUM dictionary blanks an unknown
            // dimension value instead of failing the whole query. That NULL is
            // visually ambiguous with a rolled-up cell, but all other rows remain
            // available. Ingest normally refreshes the dictionary.
            selects.push(format!("try_cast(s.\"{g}\" as {ty}) as \"{g}\""));
        }
        columns.push(CompiledColumn {
            name: g.clone(),
            grain: None,
            attribution_by_depth: vec![Attribution::Additive; n + 1],
            scope_semantics: ScopeSemantics::Direct,
            summable: false,
            mixed_flag: None,
        });
    }
    selects.push("s.row_depth".to_string());
    columns.push(CompiledColumn {
        name: "row_depth".to_string(),
        grain: None,
        attribution_by_depth: vec![Attribution::Additive; n + 1],
        scope_semantics: ScopeSemantics::Direct,
        summable: false,
        mixed_flag: None,
    });
    joins.extend(agg_joins);
    selects.extend(agg_selects);
    columns.extend(agg_columns);
    // Each value column is followed by its flag; link them by the flag's
    // absolute position now that it is known.
    selects.extend(unanimity_selects);
    let base = columns.len();
    for (i, mut c) in unanimity_columns.into_iter().enumerate() {
        if i % 2 == 0 {
            c.mixed_flag = Some(base + i + 1);
        }
        columns.push(c);
    }

    // Join schema-declared keys onto the spine only at grouping depths that
    // reach the joined dataset's key. Coarser rolled-up rows have NULL keys
    // and yield NULL attributes instead of an arbitrary entity's values.
    let mut stalest_input = vec![view.dataset.clone()];
    for (i, join) in view.joins.iter().enumerate() {
        let Some(joined_ds) = schema.dataset(&join.dataset) else {
            // Enforce the required-join contract even for callers that bypass
            // view validation. Optional joins may omit unavailable datasets.
            if !join.required {
                continue;
            }
            return Err(compile_error(
                view,
                format!("join names unknown dataset '{}'", join.dataset),
            ));
        };
        // A computed dataset has no relation to join against, so the join
        // can never be honoured, required or not.
        if joined_ds.is_computed() {
            return Err(compile_error(
                view,
                format!("join names computed dataset '{}'", join.dataset),
            ));
        }

        // The key must be on the spine *as materialized*. Testing the
        // whole grouping would reference a column the bounded spine does
        // not group by, which is a binder error rather than a NULL.
        if !join.on.iter().all(|k| materialized.contains(k)) {
            continue;
        }
        let Some(joined_grain) = joined_ds
            .grains()
            .into_iter()
            .find(|g| carries_all(joined_ds, *g, &join.on, dims))
        else {
            // Without a grain carrying every key there is no table to join.
            // Required joins fail explicitly; optional joins may be omitted.
            if !join.required {
                continue;
            }
            return Err(compile_error(
                view,
                format!(
                    "join on {:?} names keys no grain of dataset '{}' carries",
                    join.on, join.dataset
                ),
            ));
        };
        // Only datasets actually read contribute to provenance and freshness.
        stalest_input.push(join.dataset.clone());

        // Which of the joined dataset's columns this view actually wants.
        let wanted: Vec<&String> = view
            .columns
            .iter()
            .filter_map(|c| match c {
                ViewColumn::Dimension { name, .. } => Some(name),
                _ => None,
            })
            .filter(|name| joined_ds.column(name).is_some() && !view.grouping.contains(*name))
            .collect();

        // Reduce the joined table to one row per join key before joining the
        // spine. Multiple positions can carry the same instrument reference; a
        // direct join would multiply the result. `any_value` selects attributes
        // that should agree; cross-file conflict diagnostics report disagreement.
        let joined_gen = match as_of {
            crate::query::as_of::AsOf::Live => None,
            crate::query::as_of::AsOf::At(t) => {
                // Each dataset has its own generations, so the spine's
                // predicate does not apply here. Without this a
                // historical join reads every archived generation at once.
                let gens = crate::query::as_of::resolve_generations(conn, &join.dataset, *t)?;
                if let Some(oldest) = gens.iter().map(|g| g.source_time).min() {
                    resolved_as_of.insert(join.dataset.clone(), oldest);
                }
                Some(crate::query::as_of::generation_predicate(&gens))
            }
        };
        let joined_era = Era {
            kind: era.kind,
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
            "left join (select {projection} from {relation} group by {keys}) {alias} on {on}",
            projection = projection.join(", "),
            // The relation filters archive and live by generation already. Adding
            // a WHERE here would run the same tuple semi-join twice.
            relation = joined_era.relation(&join.dataset, joined_grain),
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
                        Some(g) => attribution_of(joined_ds, g, &view.grouping[..d], dims),
                        None => Attribution::Additive,
                    })
                    .collect(),
                scope_semantics: ScopeSemantics::Direct,
                summable: false,
                mixed_flag: None,
            });
        }
    }

    // Derived columns are expressions over the columns already selected.
    for c in &view.columns {
        if let ViewColumn::Derived { name, sql, .. } = c {
            // A derived expression inherits its inputs' attribution. It cannot
            // claim additive values at a depth where an input is non-attributable.
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
            // Blank derived values wherever the attribution meet is
            // NonAttributable. A bare input name in this SELECT resolves to the
            // joined aggregate's column before its neighboring masked alias, so
            // input masking alone cannot prevent a derived expression from
            // exposing a coarser measure at an invalid depth.
            let blank: Vec<String> = (0..=n)
                .filter(|d| attribution_by_depth[*d] == Attribution::NonAttributable)
                .map(|d| d.to_string())
                .collect();
            let expr = if blank.is_empty() {
                format!("({sql})")
            } else {
                format!(
                    "case when s.row_depth in ({}) then null else ({sql}) end",
                    blank.join(", ")
                )
            };
            selects.push(format!("{expr} as \"{name}\""));
            columns.push(CompiledColumn {
                name: name.clone(),
                grain: None,
                attribution_by_depth,
                scope_semantics,
                summable: false,
                mixed_flag: None,
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
    // This is a determinism backstop, not a presentation order. What holds
    // is that two queries against one generation return rows in the same
    // order. A view that wants a meaningful order should declare `sort`,
    // which is emitted ahead of these.
    //
    // The tie-breakers order on the **spine** column, `s."g"`, not on the
    // output alias. The alias is the ENUM cast, and `try_cast` maps every
    // value the ENUM does not carry to NULL — so two siblings with
    // different dimension values collapse to the same key and the order
    // stops being total exactly when a stale ENUM makes it matter most.
    // The spine holds the raw varchar, so this is total by construction,
    // and it drops the dependence on ENUM declaration order (which
    // `refresh_enum` builds from a bare `select distinct`, so it is
    // neither alphabetical nor stable across an ingest).
    //
    // Only the materialized prefix is ordered on: below the bound the
    // output column is a constant `NULL::ty` for every row, so it can
    // break no tie and the spine does not carry it either.
    let mut order_keys = vec!["s.row_depth asc".to_string()];
    for s in &view.sort {
        order_keys.push(format!(
            "\"{}\" {}",
            s.column,
            if s.descending { "desc" } else { "asc" }
        ));
    }
    let sorted_on: Vec<&str> = view.sort.iter().map(|s| s.column.as_str()).collect();
    for g in view.grouping.iter().take(depth) {
        if !sorted_on.contains(&g.as_str()) {
            order_keys.push(format!("s.\"{g}\" asc"));
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
        resolved_generation: None,
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

    /// Fixture grouped by LHU, underlying, and position, with one measure at
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
        crate::store::Catalog::new(store.writer())
            .ensure_tables()
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

    /// Rebuild each dataset's summary from its own payload tables. Raw SQL
    /// fixtures bypass publication maintenance and need this before as-of queries.
    fn rebuild_all_generations(store: &crate::store::Store, schema: &SchemaSpec) {
        for ds in &schema.datasets {
            crate::store::ddl::rebuild_generations(
                store.writer(),
                &ds.name,
                &crate::store::ddl::history_of(&ds.name, ds),
            )
            .unwrap();
        }
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

    /// Only a plain `sum` measure adds up across sibling rows. A `max`
    /// or `min` measure is additive in attribution (its value belongs to
    /// its row) yet totalling two maxima is wrong, and a derived ratio of
    /// sums is not a sum. The mark rides the snapshot's column metadata,
    /// which is what the blotter's footer reads.
    #[test]
    fn only_a_plain_sum_measure_is_marked_summable() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
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
[risk_snapshot.columns.delta01]
type = "f64"
role = "measure"
grain = "position"
[risk_snapshot.columns.peak]
type = "f64"
role = "measure"
grain = "position"
aggregate = "max"
[risk_snapshot.columns.low]
type = "f64"
role = "measure"
grain = "position"
aggregate = "min"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        let schema = SchemaSpec::from_doc(&doc).0;
        store
            .apply_schema(schema.dataset("risk_snapshot").unwrap())
            .unwrap();
        crate::store::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        let text = r#"
[v]
dataset = "risk_snapshot"
grouping = ["lhu"]
[[v.columns]]
name = "delta01"
kind = "measure"
[[v.columns]]
name = "peak"
kind = "measure"
[[v.columns]]
name = "low"
kind = "measure"
[[v.columns]]
name = "ratio"
kind = "derived"
sql = "delta01 / nullif(peak, 0)"
"#;
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        let view = ViewSpec::from_doc(&doc).0.into_iter().next().unwrap();
        let q = compile_view(
            store.writer(),
            &view,
            &schema,
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap();
        let snap = crate::query::pool::run_snapshot(
            store.writer(),
            &q,
            &view.grouping,
            geode_core::snapshot::Provenance::default(),
        )
        .unwrap();
        let summable = |name: &str| {
            let i = snap.column_index(name).unwrap();
            snap.meta_at(i).unwrap().summable
        };
        assert!(summable("delta01"), "a sum measure totals");
        assert!(!summable("peak"), "a max measure must not total");
        assert!(!summable("low"), "a min measure must not total");
        assert!(!summable("ratio"), "a derived ratio must not total");
        assert!(!summable("lhu"), "a grouping column is not a measure");
    }

    #[test]
    fn a_join_the_compiler_cannot_honour_is_an_error_not_a_silent_drop() {
        // Direct compiler callers must receive the same required-join refusal
        // as callers that run view validation first.
        let (_d, store) = fixture();
        let schema = joined_schema();
        store
            .apply_schema(schema.dataset("instrument_ref").unwrap())
            .unwrap();

        let mut unhonourable = joined_view();
        unhonourable.joins[0].dataset = "no_such_dataset".to_string();

        let err = compile_view(
            store.writer(),
            &unhonourable,
            &schema,
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("no_such_dataset"),
            "the error must name the unhonourable join: {message}"
        );

        // A join key below this query's materialized depth is valid. The
        // compiler skips the join and emits NULL for that grouping key.
        let q = joined_query(&store, &schema, 0);
        let rows = run(&store, &q, &["row_depth", "instrument_ref"]);
        assert_eq!(rows.len(), 1, "the grand total alone: {rows:?}");
        assert_eq!(
            rows[0][1], "None",
            "a key below max_depth is NULL, not an error: {rows:?}"
        );
    }

    #[test]
    fn a_view_over_a_computed_dataset_is_refused_before_any_table_is_touched() {
        let (_d, store) = fixture();
        let doc = merge_docs(
            "datasets",
            &[LayerDoc::builtin(
                "datasets",
                "[pricer]\ncomputed = true\n[pricer.columns.instrument_ref]\ntype = \"utf8\"\nrole = \"key\"\n[pricer.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"underlying\"\n",
            )
            .unwrap()],
        );
        let schema = SchemaSpec::from_doc(&doc).0;
        let view = ViewSpec {
            name: "vanilla".into(),
            dataset: "pricer".into(),
            columns: vec![geode_core::view::ViewColumn::measure("npv")],
            ..ViewSpec::default()
        };
        let err = compile_view(
            store.writer(),
            &view,
            &schema,
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap_err();
        assert!(err.to_string().contains("computed by a module"), "{err}");
    }

    /// An optional join to an unknown dataset is skipped; an optional join
    /// to a computed one is not, because the dataset is known and has no
    /// relation the compiler could ever join. `required` does not soften it.
    #[test]
    fn a_join_to_a_computed_dataset_is_refused_even_when_optional() {
        let (_d, store) = fixture();
        let doc = merge_docs(
            "datasets",
            &[LayerDoc::builtin(
                "datasets",
                r#"
[instrument_ref]
computed = true
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
            )
            .unwrap()],
        );
        let (computed, diags) = SchemaSpec::from_doc(&doc);
        assert!(
            diags
                .iter()
                .all(|d| d.severity != geode_core::config::Severity::Error),
            "the computed fixture must parse cleanly: {diags:?}"
        );
        let mut schema = schema();
        schema.datasets.extend(computed.datasets);

        let mut optional = joined_view();
        optional.joins[0].required = false;

        let err = compile_view(
            store.writer(),
            &optional,
            &schema,
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("join names computed dataset 'instrument_ref'"),
            "an optional join to a computed dataset is refused, not skipped: {message}"
        );
    }

    /// The other half of the same refusal, and the arm the unknown-dataset case
    /// above cannot reach: the joined dataset exists, its keys are on the
    /// materialized spine, and still no grain of it is keyed by them. There is
    /// no table to read, so dropping the join here would leave every column it
    /// was to supply missing from the row — blank in the blotter, with nothing
    /// on screen to distinguish it from a genuine NULL.
    #[test]
    fn a_join_no_grain_of_the_joined_dataset_can_serve_is_an_error_naming_its_keys() {
        let (_d, store) = fixture();
        let schema = joined_schema();
        store
            .apply_schema(schema.dataset("instrument_ref").unwrap())
            .unwrap();

        // `instrument_ref` declares one grain, instrument, whose dimension key
        // stops at `instrument_ref`; `underlying_ref` is neither in that key nor
        // a column of the dataset at all. Grouping by it puts it on the spine,
        // so the depth guard passes and the missing grain is what fails.
        let mut unservable = joined_view();
        unservable.grouping = vec!["underlying_ref".to_string()];
        unservable.joins[0].on = vec!["underlying_ref".to_string()];

        let err = compile_view(
            store.writer(),
            &unservable,
            &schema,
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("[\"underlying_ref\"]") && message.contains("instrument_ref"),
            "the error must name the keys no grain carries and the dataset it \
             tried to join: {message}"
        );
    }

    /// The author's opt-out, honoured at the compiler. `required = false` asks
    /// for the join to be dropped, not for the view to stop answering: erroring
    /// here would leave the view open at load and dead at every query, which is
    /// worse than the blank column this strictness removed.
    #[test]
    fn an_optional_join_naming_an_unknown_dataset_is_dropped_and_the_view_serves() {
        let (_d, store) = fixture();
        let schema = joined_schema();

        let mut optional = joined_view();
        optional.joins[0].dataset = "no_such_dataset".to_string();
        optional.joins[0].required = false;

        let q = compile_view(
            store.writer(),
            &optional,
            &schema,
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap_or_else(|e| panic!("an optional join must be dropped, not refused: {e}"));

        let rows = run(&store, &q, &["instrument_ref", "delta01", "strike"]);
        assert!(
            rows.iter().any(|r| r[1] == "Some(30.0)"),
            "the view must still answer with its own measures: {rows:?}"
        );
        assert!(
            rows.iter().all(|r| r[2] == "?"),
            "a dropped join supplies nothing, so its column is absent: {rows:?}"
        );
    }

    /// The same opt-out at the other arm: the joined dataset exists and its key
    /// is on the spine, but no grain of it is keyed by that column.
    #[test]
    fn an_optional_join_no_grain_can_serve_is_dropped_and_the_view_serves() {
        let (_d, store) = fixture();
        let schema = joined_schema();
        store
            .apply_schema(schema.dataset("instrument_ref").unwrap())
            .unwrap();

        let mut optional = joined_view();
        optional.grouping = vec!["underlying_ref".to_string()];
        optional.joins[0].on = vec!["underlying_ref".to_string()];
        optional.joins[0].required = false;

        let q = compile_view(
            store.writer(),
            &optional,
            &schema,
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap_or_else(|e| panic!("an optional join must be dropped, not refused: {e}"));

        let rows = run(&store, &q, &["underlying_ref", "delta01", "strike"]);
        assert!(
            rows.iter()
                .any(|r| r[0] == "Some(\"SPX\")" && r[1] == "Some(10.0)"),
            "the view must still answer with its own measures: {rows:?}"
        );
        assert!(
            rows.iter().all(|r| r[2] == "?"),
            "a dropped join supplies nothing, so its column is absent: {rows:?}"
        );
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
        rebuild_all_generations(&store, &schema);
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
    fn an_as_of_join_labels_each_side_with_the_instant_it_actually_read() {
        // Resolve the joined datasets to different instants. Each must retain
        // its selected timestamp so `Provenance::stalest` exposes the lagging
        // input; one shared timestamp would conceal the difference.
        let (_d, store) = fixture();
        let schema = joined_schema();
        store
            .apply_schema(schema.dataset("instrument_ref").unwrap())
            .unwrap();
        store
            .writer()
            .execute_batch(
                "delete from risk_snapshot_underlying_live;
                 -- risk is current as of 10 August
                 insert into risk_snapshot_underlying_archive values
                   ('BK0','L0','P1','C','I1','SPX', 10, 'b', 1, 1, TIMESTAMPTZ '2026-08-10 00:00:00Z');
                 -- the reference data is three weeks staler
                 insert into instrument_ref_instrument_live
                 values ('BK0','L0','P1','C','I1', 4200.0, 'b', 1, 1, TIMESTAMPTZ '2026-07-20 00:00:00Z');",
            )
            .unwrap();
        rebuild_all_generations(&store, &schema);

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

        let risk = q.resolved_as_of.get("risk_snapshot").copied();
        let reference = q.resolved_as_of.get("instrument_ref").copied();
        assert_eq!(
            risk.map(|t| t.to_rfc3339()),
            Some("2026-08-10T00:00:00+00:00".to_string()),
            "the spine's own instant"
        );
        assert_eq!(
            reference.map(|t| t.to_rfc3339()),
            Some("2026-07-20T00:00:00+00:00".to_string()),
            "and the join's, which is three weeks older"
        );
        assert_ne!(
            risk, reference,
            "the two sides must not collapse to one instant, or the \
             stalest-input rule has nothing to compare"
        );

        // The data still joins: labelling is not the only thing being
        // checked, or a compiler that returned no rows would pass.
        let rows = run(&store, &q, &["row_depth", "instrument_ref", "strike"]);
        assert!(
            rows.iter().any(|r| r[2] == "Some(4200.0)"),
            "the reference value must still be read: {rows:?}"
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
        // Tree flattening requires parents before children, including views
        // without an explicit sort. Use enough rows that incidental output order
        // is unlikely to mask a missing depth order.
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
            // On the spine column, not the output alias. The alias is the
            // ENUM cast, and `try_cast` collapses every value the ENUM
            // lacks to NULL — so ordering on it stops being total exactly
            // when a stale ENUM makes two siblings share a key.
            assert!(
                order.contains(&format!("s.\"{g}\"")),
                "grouping column {g} must break ties on the spine: {order}"
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

    /// A currency dimension carried at instrument grain. Position NPV and
    /// underlying delta differ in whether currency grouping can attribute them.
    fn carried_schema() -> SchemaSpec {
        let text = r#"
[risk_carried.columns.book]
type = "utf8"
role = "dimension"
[risk_carried.columns.lhu]
type = "utf8"
role = "dimension"
[risk_carried.columns.position_ref]
type = "utf8"
role = "key"
[risk_carried.columns.counterparty]
type = "utf8"
role = "dimension"
[risk_carried.columns.instrument_ref]
type = "utf8"
role = "key"
[risk_carried.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk_carried.columns.currency]
type = "utf8"
role = "dimension"
grain = "instrument"
[risk_carried.columns.npv]
type = "f64"
role = "measure"
grain = "position"
[risk_carried.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc).0
    }

    /// Two positions, each with one instrument and one underlying, each
    /// instrument carrying its own currency.
    fn carried_fixture() -> (tempfile::TempDir, crate::store::Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        store
            .apply_schema(carried_schema().dataset("risk_carried").unwrap())
            .unwrap();
        store
            .writer()
            .execute_batch(
                "insert into risk_carried_position_live
                   (book, lhu, position_ref, counterparty, npv,
                    batch, source_file_id, gen_id, source_time)
                 values
                   ('BK0','L0','P1','C', 100, 'b', 1, 1, now()),
                   ('BK0','L0','P2','C', 50, 'b', 1, 1, now());
                 insert into risk_carried_underlying_live
                   (book, lhu, position_ref, counterparty, instrument_ref,
                    underlying_ref, delta01, currency,
                    batch, source_file_id, gen_id, source_time)
                 values
                   ('BK0','L0','P1','C','I1','U1', 10, 'USD', 'b', 1, 1, now()),
                   ('BK0','L0','P2','C','I2','U2', 20, 'EUR', 'b', 1, 1, now());",
            )
            .unwrap();
        (dir, store)
    }

    struct CarriedRow {
        depth: i64,
        npv: Option<f64>,
        delta01: Option<f64>,
    }

    /// Compile and run a single-column-group view over `carried_fixture`.
    fn run_carried(
        store: &crate::store::Store,
        schema: &SchemaSpec,
        group: &str,
    ) -> Vec<CarriedRow> {
        let text = format!(
            "[t]\ndataset = \"risk_carried\"\ngrouping = [\"{group}\"]\n\
             [[t.columns]]\nname = \"npv\"\nkind = \"measure\"\n\
             [[t.columns]]\nname = \"delta01\"\nkind = \"measure\"\n"
        );
        let doc = merge_docs("views", &[LayerDoc::builtin("views", &text).unwrap()]);
        let view = ViewSpec::from_doc(&doc).0.into_iter().next().unwrap();
        let q = compile_view(
            store.writer(),
            &view,
            schema,
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap();
        let conn = store.writer();
        let mut stmt = conn.prepare(&q.sql).unwrap();
        let rows = stmt
            .query_map(duckdb::params_from_iter(q.params.iter()), |r| {
                Ok(CarriedRow {
                    depth: r.get("row_depth")?,
                    npv: r.get("npv")?,
                    delta01: r.get("delta01")?,
                })
            })
            .unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    /// The grand total's value for `col` — the one place both groupings
    /// below must agree, since the grand total is always additive.
    fn total(rows: &[CarriedRow], col: &str) -> f64 {
        let grand = rows.iter().find(|r| r.depth == 0).unwrap_or_else(|| {
            panic!(
                "no grand total row: {:?}",
                rows.iter().map(|r| r.depth).collect::<Vec<_>>()
            )
        });
        match col {
            "npv" => grand.npv.expect("npv grand total must not be blank"),
            "delta01" => grand
                .delta01
                .expect("delta01 grand total must not be blank"),
            other => panic!("unknown column '{other}'"),
        }
    }

    #[test]
    fn grouping_by_a_carried_dimension_sums_like_the_key_it_depends_on_and_blanks_coarser_measures()
    {
        let (_d, store) = carried_fixture();
        let schema = carried_schema();
        let by_currency = run_carried(&store, &schema, "currency");
        let by_instrument = run_carried(&store, &schema, "instrument_ref");

        // delta01 (underlying grain) sums to the same total either way.
        assert_eq!(
            total(&by_currency, "delta01"),
            total(&by_instrument, "delta01")
        );

        // npv (position grain) is NonAttributable at the currency level: NULL.
        for row in by_currency.iter().filter(|r| r.depth == 1) {
            assert!(
                row.npv.is_none(),
                "position-grain npv must be blank under a currency grouping"
            );
        }
        // and the grand total still carries it.
        assert!(by_currency.iter().any(|r| r.depth == 0 && r.npv.is_some()));

        // A view that selects only the position-grain measure has no
        // aggregate whose own grouping covers "currency" (npv's grain,
        // Position, does not carry it) — the spine for the currency
        // level then falls back to `finest_carrying`/`carries_all`,
        // which only `DatasetSpec::carries` (not the raw dimension key)
        // can name as carrying a currency column at all.
        let text = "[t2]\ndataset = \"risk_carried\"\ngrouping = [\"currency\"]\n\
                     [[t2.columns]]\nname = \"npv\"\nkind = \"measure\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        let npv_only = ViewSpec::from_doc(&doc).0.into_iter().next().unwrap();
        let q = compile_view(
            store.writer(),
            &npv_only,
            &schema,
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap_or_else(|e| panic!("a currency-only spine must still compile: {e}"));
        let rows = run(&store, &q, &["row_depth", "currency"]);
        assert!(
            rows.iter().any(|r| r[0] == "Some(1.0)"),
            "the currency level must exist even with no covering measure: {rows:?}"
        );
    }

    /// A numeric carried dimension — a strike a desk groups by — is
    /// carried exactly like a string one: the compiler's vocabulary is
    /// "what some declared grain carries", not "what has an ENUM
    /// dictionary", so `role = "dimension"` on an `f64` column produces a
    /// grouping level per distinct value, selected as TEXT (the varchar
    /// cast below is what the blotter's tree cell can read). Pins the
    /// schema side too: with the old categorical default (dimension ⇒
    /// categorical, no type check) `apply_schema` would have sent this
    /// column for ENUM interning.
    #[test]
    fn grouping_by_a_numeric_carried_dimension_produces_a_level_with_its_values() {
        let text = r#"
[risk_num.columns.book]
type = "utf8"
role = "dimension"
[risk_num.columns.lhu]
type = "utf8"
role = "dimension"
[risk_num.columns.position_ref]
type = "utf8"
role = "key"
[risk_num.columns.counterparty]
type = "utf8"
role = "dimension"
[risk_num.columns.instrument_ref]
type = "utf8"
role = "key"
[risk_num.columns.strike]
type = "f64"
role = "dimension"
grain = "instrument"
[risk_num.columns.vega]
type = "f64"
role = "measure"
grain = "instrument"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        let (schema, diags) = SchemaSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset("risk_num").unwrap();
        assert!(
            !ds.column("strike").unwrap().categorical,
            "a numeric dimension must not be interned"
        );

        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        store.apply_schema(ds).unwrap();
        store
            .writer()
            .execute_batch(
                "insert into risk_num_instrument_live
                   (book, lhu, position_ref, counterparty, instrument_ref, strike, vega,
                    batch, source_file_id, gen_id, source_time)
                 values
                   ('BK0','L0','P1','C','I1', 4200.0, 10, 'b', 1, 1, now()),
                   ('BK0','L0','P2','C','I2', 4200.0, 20, 'b', 1, 1, now()),
                   ('BK0','L0','P3','C','I3', 4500.0, 5, 'b', 1, 1, now());",
            )
            .unwrap();

        let view_text = "[t]\ndataset = \"risk_num\"\ngrouping = [\"strike\"]\n\
                         [[t.columns]]\nname = \"vega\"\nkind = \"measure\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", view_text).unwrap()]);
        let view = ViewSpec::from_doc(&doc).0.into_iter().next().unwrap();
        let q = compile_view(
            store.writer(),
            &view,
            &schema,
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap_or_else(|e| panic!("a strike grouping must compile: {e}"));
        // The load-bearing fact for the blotter: a non-interned grouping
        // column is selected as VARCHAR, whatever its storage type, because
        // the snapshot reads a grouping column as text (`Snapshot::text_in`
        // downcasts to a string or dictionary array and nothing else) — a
        // raw DOUBLE here would paint every strike level blank. `run`'s
        // f64-first read would coerce "4200.0" back to a number and hide
        // that, so the column is read as text, strictly.
        assert!(
            q.sql.contains("s.\"strike\"::varchar as \"strike\""),
            "the strike level must be selected as text:\n{}",
            q.sql
        );
        let conn = store.writer();
        let mut stmt = conn.prepare(&q.sql).unwrap();
        let mut rows: Vec<(i64, Option<String>, Option<f64>)> = stmt
            .query_map(duckdb::params_from_iter(q.params.iter()), |r| {
                Ok((r.get("row_depth")?, r.get("strike")?, r.get("vega")?))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        rows.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(
            rows,
            vec![
                (0, None, Some(35.0)),
                (1, Some("4200.0".to_string()), Some(30.0)),
                (1, Some("4500.0".to_string()), Some(5.0)),
            ],
            "one level per distinct strike, summed, under a grand total"
        );
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

    /// Derived dimension fixture: `desk` maps values from `book`.
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
    fn a_derived_dimension_finer_than_a_measures_grain_blanks_that_measure() {
        // Resolve `region` to its source `underlying_ref` before checking grain
        // attribution. Underlying delta can be grouped by region; position PnL
        // cannot be divided among its underlyings' regions without inventing
        // an allocation. The fixture must exercise both measures together.
        let (_d, store) = fixture();
        let dims = {
            let doc = merge_docs(
                "dimensions",
                &[LayerDoc::builtin(
                    "dimensions",
                    "[region]\nfrom = \"underlying_ref\"\n\
                     [region.values]\nAMER = [\"SPX\"]\nEMEA = [\"RUT\"]\n",
                )
                .unwrap()],
            );
            DerivedDimensions::from_doc(&doc).0
        };
        let view = {
            let text = r#"
[by_region]
dataset = "risk_snapshot"
grouping = ["region"]
[[by_region.columns]]
name = "delta01"
kind = "measure"
[[by_region.columns]]
name = "daily_trading_pnl"
kind = "measure"
"#;
            let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
            ViewSpec::from_doc(&doc).0.into_iter().next().unwrap()
        };

        let q = compile_view(
            store.writer(),
            &view,
            &schema(),
            &Scope::default(),
            &dims,
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap();

        let of = |name: &str| {
            q.columns
                .iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("no column {name}"))
        };
        assert_eq!(
            of("delta01").attribution_by_depth[1],
            Attribution::Additive,
            "an underlying-grain measure is attributable at a level keyed \
             by an underlying-derived dimension"
        );
        assert_eq!(
            of("daily_trading_pnl").attribution_by_depth[1],
            Attribution::NonAttributable,
            "a position-grain measure is not: the position grain does not \
             carry underlying_ref, so no share of the PnL belongs here"
        );

        // And the value is blanked, not merely marked.
        let rows = run(
            &store,
            &q,
            &["row_depth", "region", "delta01", "daily_trading_pnl"],
        );
        let region_rows: Vec<&Vec<String>> = rows.iter().filter(|r| r[0] == "Some(1.0)").collect();
        assert!(
            !region_rows.is_empty(),
            "the fixture must produce region rows: {rows:?}"
        );
        for r in &region_rows {
            assert_eq!(r[3], "None", "PnL must be blank at the region level: {r:?}");
        }
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

    /// Fixture covering NULL book and LHU values, multiple archived generations,
    /// a dimension value found only in history, and an incomplete ENUM dictionary.
    /// These cases exercise historical reads beyond the ordinary live-data path.
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
        // Build ENUMs from live only to verify that historical reads tolerate
        // a dictionary missing archived values. Production refresh unions
        // archive values too, but query correctness must not depend on a
        // complete dictionary. Only create enums for columns this grain carries.
        for col in
            crate::store::ddl::categorical_columns(schema().dataset("risk_snapshot").unwrap())
                .into_iter()
                .filter(|c| Grain::Underlying.key_columns().contains(c))
        {
            let live =
                crate::store::ddl::table_name("risk_snapshot", Grain::Underlying, TableKind::Live);
            crate::store::ddl::refresh_enum(conn, "risk_snapshot", col, &live, &live).unwrap();
        }
        rebuild_all_generations(&store, &schema());
        (dir, store)
    }

    #[test]
    fn as_of_survives_a_value_that_has_since_left_live() {
        // Live ENUMs can omit retired values present in archived rows.
        // Historical reads must bypass those ENUM casts to retain such values.
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
        // A published file can contain a partition at only one grain: a cash-only
        // book has no underlying rows. Historical resolution must include it
        // without requiring membership in the spine's grain.
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
        // A measure declared at this grain is directly filterable even when
        // it is not a key dimension. Marking it SemiJoined would describe
        // an existence filter that the query does not perform.
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
        // The finer predicate appears inside a semi-join after the direct book
        // predicate. Bound values must follow that SQL order so each predicate
        // receives its own value and coarse measures remain populated.
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
        // Historical semi-join probes must use the selected era too. Reading
        // unfiltered live rows here would mix current values into history.
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
        rebuild_all_generations(&store, &schema());

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
        // The newest generation lives only in live. Historical resolution
        // must include it, including for a partition published only once.
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
        rebuild_all_generations(&store, &schema());
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
        // Freshness reports the oldest selected partition, matching live rollups.
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
            categorical: false,
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
        // The pair table stores `least(u1, u2)` as `underlying_ref`. Filtering
        // that column alone misses an underlying that sorts second in its pair.
        // Both the spine and the semi-join must retain SPX and its position PnL.
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
        // An unscoped spine must include both underlying rows. Scanning only
        // the pair table's `least` key would omit SPX while still including its
        // delta in the grand total.
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
        // A canonical pair has no attributable share at underlying level;
        // instrument-level and coarser rows can aggregate it.
        let (_d, store) = pair_fixture();
        let mut v = view();
        v.columns.push(ViewColumn::measure("cross_gamma02"));
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
        // Verify NULLs through `Snapshot`, the module-facing result, as well
        // as through DuckDB's row API. Raw Arrow value buffers can contain
        // zero behind a NULL; consumers must respect the validity bitmap.
        use geode_core::snapshot::{ColumnMeta, Provenance, Snapshot};

        let (_d, store) = pair_fixture();
        let mut v = view();
        v.columns.push(ViewColumn::measure("cross_gamma02"));
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
                summable: c.summable,
                mixed_flag: None,
            })
            .collect();
        // DuckDB emits `row_depth` as Int32. Check the real result type so
        // snapshot depth reads are exercised beyond Int64-only fixtures.
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
        let snap = Snapshot::from_batches(batches, meta, v.grouping.clone(), Provenance::default())
            .unwrap();

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
                        "row {row} at depth {depth} reads {v} where attribution requires NULL"
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
        // An expression over cross gamma inherits its non-attributable depths.
        // The renderer uses that marker to decide where values can be summed.
        let (_d, store) = pair_fixture();
        let mut v = view();
        v.columns.push(ViewColumn::measure("cross_gamma02"));
        v.columns.push(ViewColumn::derived(
            "cg_per_delta",
            "cross_gamma02 / nullif(delta01, 0)",
        ));
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
    fn a_derived_column_is_blanked_where_its_inputs_are() {
        // Read derived values as well as markers. SQL resolves a bare input
        // name to the aggregate column before the adjacent masked alias, so
        // the expression itself must be masked at non-attributable depths.
        let (_d, store) = pair_fixture();
        let mut v = view();
        v.columns.push(ViewColumn::measure("cross_gamma02"));
        v.columns
            .push(ViewColumn::derived("cg_copy", "cross_gamma02"));
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

        let rows = run(&store, &q, &["row_depth", "cross_gamma02", "cg_copy"]);
        assert!(!rows.is_empty(), "the fixture must produce rows");
        let mut blanked = 0;
        for r in &rows {
            if r[1] == "None" {
                assert_eq!(
                    r[2], "None",
                    "row_depth {} blanks the measure but not the expression over it: {rows:?}",
                    r[0]
                );
                blanked += 1;
            }
        }
        assert!(
            blanked >= 2,
            "the fixture must actually blank some rows, or this asserts nothing: {rows:?}"
        );
    }

    #[test]
    fn an_apostrophe_in_a_comment_does_not_hide_the_columns_referenced() {
        // An apostrophe in a SQL comment must not start a string token and
        // hide subsequent identifiers. Missing a referenced input could grant
        // additive attribution at depths where it is invalid.
        let (_d, store) = pair_fixture();
        for sql in [
            "-- don't sum this across pairs\n cross_gamma02 * 2",
            "/* the desk's own scaling */ cross_gamma02 * 2",
            "cross_gamma02 * 2 -- can't total this",
        ] {
            let mut v = view();
            v.columns.push(ViewColumn::measure("cross_gamma02"));
            v.columns.push(ViewColumn::derived("scaled", sql));
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
            };
            assert_eq!(
                of("scaled").attribution_by_depth,
                of("cross_gamma02").attribution_by_depth,
                "sql: {sql}"
            );
        }
    }

    #[test]
    fn a_comment_does_not_drag_in_columns_the_expression_never_names() {
        // Use an additive input to distinguish correct comment stripping from
        // a conservative unbalanced-quote fallback. Pulling in every column
        // would also include the pair-grain measure and wrongly blank this
        // expression at depths where its actual input remains additive.
        let (_d, store) = pair_fixture();
        let mut v = view();
        v.columns.push(ViewColumn::measure("cross_gamma02"));
        v.columns.push(ViewColumn::derived(
            "scaled_delta",
            "-- the desk's own scaling\n delta01 * 2",
        ));
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
        };
        assert_eq!(
            of("scaled_delta").attribution_by_depth,
            of("delta01").attribution_by_depth,
            "an expression over delta01 inherits delta01, not cross gamma"
        );
        assert_ne!(
            of("delta01").attribution_by_depth,
            of("cross_gamma02").attribution_by_depth,
            "precondition: the two differ, or this asserts nothing"
        );
    }

    #[test]
    fn unbalanced_quoting_fails_toward_the_weakest_claim() {
        // If the scan loses its place the token list cannot be trusted.
        // Claiming `Additive` about an expression nobody analysed is the
        // one outcome that must not happen.
        let (_d, store) = pair_fixture();
        let mut v = view();
        v.columns.push(ViewColumn::measure("cross_gamma02"));
        // A lone apostrophe outside a comment. An unterminated block
        // comment does *not* reach this guard — it is stripped — so
        // the input has to be unbalanced quoting itself.
        v.columns
            .push(ViewColumn::derived("odd", "cross_gamma02 'unterminated"));
        let q = compile_view(
            store.writer(),
            &v,
            &schema_with_pairs(),
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        );
        // The expression itself may or may not bind; what matters is that
        // when it does compile, the marker is not the strongest one.
        if let Ok(q) = q
            && let Some(odd) = q.columns.iter().find(|c| c.name == "odd")
        {
            assert!(
                odd.attribution_by_depth
                    .iter()
                    .any(|a| *a != Attribution::Additive),
                "an unanalysable expression must not claim Additive everywhere: {:?}",
                odd.attribution_by_depth
            );
        }
    }

    #[test]
    fn a_derived_column_over_one_measure_carries_exactly_that_measures_markers() {
        // The other half: the meet must be able to say "additive", or the
        // rule above is just "always blank". Over a single input the
        // answer is not a judgement call — it is that input's markers,
        // level for level, including the levels where they are additive.
        let (_d, store) = fixture();
        let mut v = view();
        v.columns
            .push(ViewColumn::derived("delta_doubled", "delta01 * 2"));
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
        v.columns.push(ViewColumn::derived("one", "1"));
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
        // A string mentioning a column must not inherit that column's markers.
        // Include a separator after the name: it exercises the string guard
        // before the closing quote clears the token buffer.
        let (_d, store) = fixture();
        let mut v = view();
        v.columns
            .push(ViewColumn::derived("label", "'daily_trading_pnl per unit'"));
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
        // An unknown ENUM value must not make the entire query fail. Ingest
        // refreshes dictionaries, but reads must tolerate an incomplete one.
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
        // A `delta01 > 5` scope at position grain must probe the underlying
        // table, where that measure exists. Choosing the spine grain could
        // reference a column absent from its table.
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
        // A cash position has trading PnL and no underlying rows. Its 900 must
        // appear below the total so the visible children sum to their parent.
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
        // Without measure aggregates, build the spine by scanning the finest
        // grain carrying the grouping columns.
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
        // A text filter on `underlying_ref` narrows greeks but cannot be applied
        // at position grain. Mark the PnL attribution rather than silently
        // presenting filtered greeks beside apparently filtered position PnL.
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
        // A predicate on `desk` must evaluate the derived mapping; no payload
        // table contains that column or stores `EU` as its source book value.
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

    /// `underlying_ref` made textual, with an ENUM type actually built —
    /// shared setup for the two tests below.
    fn schema_with_a_textual_dictionary(store: &crate::store::Store) -> SchemaSpec {
        let mut s = schema();
        s.datasets[0]
            .columns
            .iter_mut()
            .find(|c| c.name == "underlying_ref")
            .unwrap()
            .textual = true;
        let live =
            crate::store::ddl::table_name("risk_snapshot", Grain::Underlying, TableKind::Live);
        crate::store::ddl::refresh_enum(
            store.writer(),
            "risk_snapshot",
            "underlying_ref",
            &live,
            &live,
        )
        .unwrap();
        s
    }

    #[test]
    fn compile_view_over_two_grains_resolves_the_dictionary_once_per_statement() {
        // Both measure grains cover this grouping, so the spine fallback does
        // not run. The first grain warms the dataset-wide dictionary cache;
        // the second grain and interned-column check can reuse it.
        //
        // A final lookup count proves reuse but cannot detect a bypass at a
        // later site already warmed by the first. The no-measures and no-text
        // fixtures below isolate the spine and interned-column consumers.
        let (_d, store) = fixture();
        let s = schema_with_a_textual_dictionary(&store);

        let scope = Scope {
            text: Some("SPX".into()),
            ..Scope::default()
        };
        let mut cache = DictionaryCache::default();
        compile_view_with_cache(
            store.writer(),
            &view(),
            &s,
            &scope,
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
            &mut cache,
        )
        .unwrap();
        assert_eq!(
            cache.lookups, 2,
            "one enum_types lookup plus one dictionary match for the schema's one \
             categorical textual column with an existing type -- not once per \
             measure grain plus the interned-columns check"
        );
    }

    #[test]
    fn compile_view_with_no_measures_resolves_the_dictionary_once_via_the_spine() {
        // With no measures, the spine fallback is the first dictionary-cache
        // consumer and the interned-column check is the only later consumer.
        // This isolates the spine's cache use: a measure-grain lookup would
        // otherwise warm the dataset-wide entry before the spine reads it.
        let (_d, store) = fixture();
        let s = schema_with_a_textual_dictionary(&store);
        let mut v = view();
        v.columns.clear();

        let scope = Scope {
            text: Some("SPX".into()),
            ..Scope::default()
        };
        let mut cache = DictionaryCache::default();
        compile_view_with_cache(
            store.writer(),
            &v,
            &s,
            &scope,
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
            &mut cache,
        )
        .unwrap();
        assert_eq!(
            cache.lookups, 2,
            "the spine's own compile_scope_cached call must resolve through the \
             shared cache, not a throwaway one of its own"
        );
    }

    #[test]
    fn compile_view_with_no_text_scope_resolves_the_dictionary_once_via_the_interned_check() {
        // With no text scope, neither grain consults the dictionary cache.
        // The live interned-column check is its only consumer; a lookup count
        // of one therefore proves that this site uses the shared cache.
        let (_d, store) = fixture();
        let s = schema_with_a_textual_dictionary(&store);

        let mut cache = DictionaryCache::default();
        compile_view_with_cache(
            store.writer(),
            &view(),
            &s,
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
            &mut cache,
        )
        .unwrap();
        assert_eq!(
            cache.lookups, 1,
            "the interned-columns check's own cache.enum_types call is the only \
             catalog consumer when there is no text scope"
        );
    }
    /// `strike` and `expiry` carried at instrument grain, a position-grain
    /// and an underlying-grain measure. Only the position and underlying
    /// tables exist, so an ungrouped `strike` is read from the underlying one.
    fn unanimity_schema() -> SchemaSpec {
        let text = r#"
[risk_u.columns.book]
type = "utf8"
role = "dimension"
[risk_u.columns.lhu]
type = "utf8"
role = "dimension"
[risk_u.columns.position_ref]
type = "utf8"
role = "key"
[risk_u.columns.counterparty]
type = "utf8"
role = "dimension"
[risk_u.columns.instrument_ref]
type = "utf8"
role = "key"
[risk_u.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk_u.columns.strike]
type = "f64"
role = "dimension"
grain = "instrument"
[risk_u.columns.expiry]
type = "utf8"
role = "dimension"
grain = "instrument"
[risk_u.columns.npv]
type = "f64"
role = "measure"
grain = "position"
[risk_u.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc).0
    }

    /// One book, three LHUs:
    /// - L0/P1: two instruments with different strikes (100, 110), one expiry.
    /// - L0/P2: one instrument on two underlyings, strike 100 on both rows.
    /// - L1/P3: a NULL strike beside a 90, one expiry.
    /// - L1/P4: every strike and expiry NULL.
    /// - L2/P5: cash only — a position row and no underlying rows at all.
    fn unanimity_fixture() -> (tempfile::TempDir, crate::store::Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        store
            .apply_schema(unanimity_schema().dataset("risk_u").unwrap())
            .unwrap();
        store
            .writer()
            .execute_batch(
                "insert into risk_u_position_live
                   (book, lhu, position_ref, counterparty, npv,
                    batch, source_file_id, gen_id, source_time)
                 values
                   ('BK0','L0','P1','C', 1, 'b', 1, 1, now()),
                   ('BK0','L0','P2','C', 2, 'b', 1, 1, now()),
                   ('BK0','L1','P3','C', 3, 'b', 1, 1, now()),
                   ('BK0','L1','P4','C', 4, 'b', 1, 1, now()),
                   ('BK0','L2','P5','C', 5, 'b', 1, 1, now());
                 insert into risk_u_underlying_live
                   (book, lhu, position_ref, counterparty, instrument_ref,
                    underlying_ref, delta01, strike, expiry,
                    batch, source_file_id, gen_id, source_time)
                 values
                   ('BK0','L0','P1','C','I1','SPX', 1, 100, '2027-03', 'b', 1, 1, now()),
                   ('BK0','L0','P1','C','I2','SPX', 1, 110, '2027-03', 'b', 1, 1, now()),
                   ('BK0','L0','P2','C','I3','SPX', 1, 100, '2027-03', 'b', 1, 1, now()),
                   ('BK0','L0','P2','C','I3','RUT', 1, 100, '2027-03', 'b', 1, 1, now()),
                   ('BK0','L1','P3','C','I4','NDX', 1, NULL, '2027-06', 'b', 1, 1, now()),
                   ('BK0','L1','P3','C','I5','NDX', 1, 90, '2027-06', 'b', 1, 1, now()),
                   ('BK0','L1','P4','C','I6','NDX', 1, NULL, NULL, 'b', 1, 1, now()),
                   ('BK0','L1','P4','C','I7','NDX', 1, NULL, NULL, 'b', 1, 1, now());",
            )
            .unwrap();
        (dir, store)
    }

    /// A view over `risk_u` grouped by `grouping`, showing `measures` and
    /// then `strike` and `expiry` as dimension columns.
    fn unanimity_view(grouping: &str, measures: &[&str]) -> ViewSpec {
        let mut text = format!("[u]\ndataset = \"risk_u\"\ngrouping = {grouping}\n");
        for m in measures {
            text.push_str(&format!("[[u.columns]]\nname = \"{m}\"\n"));
        }
        text.push_str(
            "[[u.columns]]\nname = \"strike\"\nkind = \"dimension\"\n\
             [[u.columns]]\nname = \"expiry\"\nkind = \"dimension\"\n",
        );
        let doc = merge_docs("views", &[LayerDoc::builtin("views", &text).unwrap()]);
        ViewSpec::from_doc(&doc).0.into_iter().next().unwrap()
    }

    fn compile_unanimity(
        store: &crate::store::Store,
        view: &ViewSpec,
        max_depth: usize,
    ) -> CompiledQuery {
        let schema = unanimity_schema();
        assert!(
            view.validate(&schema, &DerivedDimensions::default())
                .iter()
                .all(|d| d.severity != geode_core::config::Severity::Error),
            "the fixture view must validate"
        );
        compile_view(
            store.writer(),
            view,
            &schema,
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            max_depth,
        )
        .unwrap_or_else(|e| panic!("compile failed at depth {max_depth}: {e}"))
    }

    /// What one tree row shows for one ungrouped dimension.
    #[derive(Debug, Clone, PartialEq)]
    enum Shown {
        Value(String),
        Mixed,
        Blank,
    }

    /// Each row as `(tree path, strike, expiry)`, the path being the
    /// grouping values present at that depth joined with `/` ("" for the
    /// grand total). Reads the value and the flag straight off the
    /// database, so a flag that is NULL rather than false fails here.
    fn unanimity_rows(
        store: &crate::store::Store,
        q: &CompiledQuery,
    ) -> Vec<(String, Shown, Shown)> {
        let conn = store.writer();
        let mut stmt = conn
            .prepare(&q.sql)
            .unwrap_or_else(|e| panic!("prepare failed: {e}\n{}", q.sql));
        let grouping = q.grouping.clone();
        let rows = stmt
            .query_map(duckdb::params_from_iter(q.params.iter()), |r| {
                let depth: i64 = r.get("row_depth")?;
                let path: Vec<String> = grouping
                    .iter()
                    .take(depth as usize)
                    .map(|g| {
                        r.get::<_, Option<String>>(g.as_str())
                            .map(|v| v.unwrap_or_default())
                    })
                    .collect::<Result<_, _>>()?;
                let shown = |col: &str| -> duckdb::Result<Shown> {
                    // In the column's own type: `strike` is a DOUBLE,
                    // `expiry` text.
                    let value = match r.get::<_, duckdb::types::Value>(col)? {
                        duckdb::types::Value::Null => None,
                        duckdb::types::Value::Double(v) => Some(v.to_string()),
                        duckdb::types::Value::Text(v) => Some(v),
                        other => panic!("{col} arrived as {other:?}"),
                    };
                    let flag: bool = r.get(mixed_flag_name(col).as_str())?;
                    Ok(match (value, flag) {
                        (Some(v), false) => Shown::Value(v),
                        (None, true) => Shown::Mixed,
                        (None, false) => Shown::Blank,
                        (Some(v), true) => panic!("{col} is both {v} and mixed"),
                    })
                };
                Ok((path.join("/"), shown("strike")?, shown("expiry")?))
            })
            .unwrap_or_else(|e| panic!("execute failed: {e}\n{}", q.sql));
        rows.map(|r| r.unwrap()).collect()
    }

    fn shown_at(rows: &[(String, Shown, Shown)], path: &str) -> (Shown, Shown) {
        let found: Vec<_> = rows.iter().filter(|r| r.0 == path).collect();
        assert_eq!(found.len(), 1, "one row at '{path}': {rows:?}");
        (found[0].1.clone(), found[0].2.clone())
    }

    fn value(v: &str) -> Shown {
        Shown::Value(v.to_string())
    }

    /// The rule at every row of a position tree: a value only where every
    /// row beneath has that one value, the marker where they disagree. P1
    /// holds two instruments struck at 100 and 110 — `any_value` would paint
    /// either, a plausible wrong strike; here it says mixed. P2's instrument
    /// sits on two underlying rows with the same strike: that is agreement.
    #[test]
    fn an_ungrouped_dimension_shows_a_value_only_where_every_row_under_it_agrees() {
        let (_d, store) = unanimity_fixture();
        let view = unanimity_view(r#"["lhu", "position_ref"]"#, &["delta01", "npv"]);
        let rows = unanimity_rows(&store, &compile_unanimity(&store, &view, usize::MAX));

        assert_eq!(shown_at(&rows, "L0/P1").0, Shown::Mixed, "100 and 110");
        assert_eq!(shown_at(&rows, "L0/P2").0, value("100"), "one strike");
        assert_eq!(shown_at(&rows, "L0").0, Shown::Mixed, "P1 disagrees");
        assert_eq!(
            shown_at(&rows, "L0"),
            (Shown::Mixed, value("2027-03")),
            "every L0 row shares an expiry, so the LHU shows it"
        );
        assert_eq!(shown_at(&rows, "").0, Shown::Mixed, "the grand total");
    }

    /// A NULL beside a value is mixed, not that value: showing 90 for P3
    /// would claim it for the instrument that has no strike. Every row NULL
    /// is blank, and so is a position with no rows at the grain at all (the
    /// cash-only P5) — blank there, not mixed and not an error.
    #[test]
    fn a_null_beside_a_value_is_mixed_and_no_value_at_all_is_blank() {
        let (_d, store) = unanimity_fixture();
        let view = unanimity_view(r#"["lhu", "position_ref"]"#, &["delta01", "npv"]);
        let rows = unanimity_rows(&store, &compile_unanimity(&store, &view, usize::MAX));

        assert_eq!(
            shown_at(&rows, "L1/P3"),
            (Shown::Mixed, value("2027-06")),
            "NULL beside 90 is mixed"
        );
        assert_eq!(
            shown_at(&rows, "L1/P4"),
            (Shown::Blank, Shown::Blank),
            "every row NULL"
        );
        assert_eq!(
            shown_at(&rows, "L1"),
            (Shown::Mixed, Shown::Mixed),
            "P3's expiry beside P4's NULLs"
        );
        assert_eq!(
            shown_at(&rows, "L2/P5"),
            (Shown::Blank, Shown::Blank),
            "no rows at the grain"
        );
        assert_eq!(shown_at(&rows, "L2"), (Shown::Blank, Shown::Blank));
    }

    /// Validation judged the column against the whole grouping, so the
    /// compiler must serve it at every bound, the grand-total-only query
    /// included, and the rows it does materialize say the same thing they
    /// say unbounded. Grouped through the underlying level too, where the
    /// aggregate joins on a key the position table does not carry.
    #[test]
    fn an_ungrouped_dimension_compiles_and_agrees_at_every_bounded_depth() {
        let (_d, store) = unanimity_fixture();
        let view = unanimity_view(
            r#"["lhu", "underlying_ref", "position_ref"]"#,
            &["npv", "delta01"],
        );
        let full = unanimity_rows(&store, &compile_unanimity(&store, &view, usize::MAX));
        assert_eq!(shown_at(&full, "L0/SPX/P1").0, Shown::Mixed);
        assert_eq!(shown_at(&full, "L0/RUT/P2").0, value("100"));
        assert_eq!(shown_at(&full, "L0/RUT").0, value("100"));
        assert_eq!(shown_at(&full, "L0/SPX").0, Shown::Mixed);
        for max_depth in 0..=3 {
            let bounded = unanimity_rows(&store, &compile_unanimity(&store, &view, max_depth));
            assert!(!bounded.is_empty());
            for row in &bounded {
                assert!(
                    full.contains(row),
                    "depth bound {max_depth}: {row:?} differs from the unbounded tree"
                );
            }
        }
        let total = unanimity_rows(&store, &compile_unanimity(&store, &view, 0));
        assert_eq!(total, vec![("".to_string(), Shown::Mixed, Shown::Mixed)]);
    }

    /// Adding a display column must not add or remove tree rows. With only a
    /// position-grain measure the dimension is read from a CTE of its own,
    /// which joins the spine but never feeds it; with an underlying measure
    /// it rides that measure's scan instead of a second one.
    #[test]
    fn an_ungrouped_dimension_neither_changes_the_rows_nor_scans_twice() {
        let (_d, store) = unanimity_fixture();
        let schema = unanimity_schema();
        let bare_text = "[b]\ndataset = \"risk_u\"\ngrouping = [\"lhu\", \"position_ref\"]\n\
                         [[b.columns]]\nname = \"npv\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", bare_text).unwrap()]);
        let bare = ViewSpec::from_doc(&doc).0.into_iter().next().unwrap();
        let bare_q = compile_view(
            store.writer(),
            &bare,
            &schema,
            &Scope::default(),
            &DerivedDimensions::default(),
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap();
        let bare_rows = run(
            &store,
            &bare_q,
            &["row_depth", "lhu", "position_ref", "npv"],
        );

        let with = unanimity_view(r#"["lhu", "position_ref"]"#, &["npv"]);
        let with_q = compile_unanimity(&store, &with, usize::MAX);
        assert!(
            with_q.sql.contains("dim_measures_underlying"),
            "{}",
            with_q.sql
        );
        let with_rows = run(
            &store,
            &with_q,
            &["row_depth", "lhu", "position_ref", "npv"],
        );
        assert_eq!(with_rows, bare_rows, "the same tree, row for row");

        let folded = compile_unanimity(
            &store,
            &unanimity_view(r#"["lhu", "position_ref"]"#, &["npv", "delta01"]),
            usize::MAX,
        );
        assert_eq!(
            folded.sql.matches("risk_u_underlying_live").count(),
            1,
            "one scan of the underlying table: {}",
            folded.sql
        );
        assert!(!folded.sql.contains("dim_"), "{}", folded.sql);
    }

    /// The flag reaches the snapshot linked to its column by index, the value
    /// stays NULL beneath it, and the column claims nothing it cannot: not
    /// summable, additive at every depth.
    #[test]
    fn the_mixed_flag_reaches_the_snapshot_beside_a_null_value() {
        let (_d, store) = unanimity_fixture();
        let view = unanimity_view(r#"["lhu", "position_ref"]"#, &["delta01", "npv"]);
        let q = compile_unanimity(&store, &view, usize::MAX);
        let snap =
            crate::query::pool::run_snapshot(store.writer(), &q, &q.grouping, Default::default())
                .unwrap();
        let strike = snap.column_index("strike").unwrap();
        let flag = snap.column_index("strike#mixed").unwrap();
        let meta = snap.meta_at(strike).unwrap();
        assert_eq!(meta.mixed_flag, Some(flag));
        assert!(!meta.summable);
        assert!(
            meta.attribution_by_depth
                .iter()
                .all(|a| *a == Attribution::Additive)
        );
        let depth = snap.column_index("row_depth").unwrap();
        let root = (0..snap.rows())
            .find(|r| snap.i64_at(depth, *r) == Some(0))
            .unwrap();
        assert!(snap.is_mixed_at(strike, root));
        assert_eq!(snap.display_at(strike, root), None, "NULL beneath the flag");
        // A numeric dimension reaches the blotter as a number, so it sorts
        // as one: as text "100" would sort before "95".
        assert!(
            (0..snap.rows()).any(|r| snap.f64_at(strike, r) == Some(100.0)),
            "strike arrives as a number"
        );
        assert!(snap.meta_at(flag).unwrap().mixed_flag.is_none());
    }

    /// Validation drops an optional unreachable dimension and refuses a
    /// required one; the compiler, reached without that gate, does the same
    /// rather than selecting nothing for it silently.
    #[test]
    fn an_unreachable_ungrouped_dimension_is_dropped_if_optional_and_an_error_if_required() {
        let (_d, store) = unanimity_fixture();
        // Without delta01 the dataset declares only the position grain,
        // which stores no strike: no table can supply it.
        let mut schema = unanimity_schema();
        schema
            .datasets
            .iter_mut()
            .find(|d| d.name == "risk_u")
            .unwrap()
            .columns
            .retain(|c| c.name != "delta01");
        for (required, ok) in [(false, true), (true, false)] {
            let text = format!(
                "[u]\ndataset = \"risk_u\"\ngrouping = [\"lhu\"]\n\
                 [[u.columns]]\nname = \"npv\"\n\
                 [[u.columns]]\nname = \"strike\"\nkind = \"dimension\"\nrequired = {required}\n"
            );
            let doc = merge_docs("views", &[LayerDoc::builtin("views", &text).unwrap()]);
            let view = ViewSpec::from_doc(&doc).0.into_iter().next().unwrap();
            let got = compile_view(
                store.writer(),
                &view,
                &schema,
                &Scope::default(),
                &DerivedDimensions::default(),
                &crate::query::as_of::AsOf::Live,
                usize::MAX,
            );
            match got {
                Ok(q) => {
                    assert!(ok, "a required unreachable column must not compile");
                    assert!(q.columns.iter().all(|c| c.name != "strike"));
                }
                Err(e) => {
                    assert!(!ok, "an optional one is dropped, not an error: {e}");
                    assert!(e.to_string().contains("strike"), "{e}");
                }
            }
        }
    }

    /// A view without ungrouped dimensions needs no unanimity aggregates.
    /// Compare the full-depth demo query against `testdata/demo_tree_view.sql`
    /// after removing any ungrouped dimensions from the demo view. Update the
    /// fixture, including its trailing newline, when the SQL shape changes.
    #[test]
    fn a_view_with_no_ungrouped_dimension_compiles_to_the_sql_it_always_has() {
        let schema = SchemaSpec::from_doc(&merge_docs(
            "datasets",
            &[LayerDoc::builtin(
                "datasets",
                include_str!("../../../../examples/demo-config/datasets.toml"),
            )
            .unwrap()],
        ))
        .0;
        let mut tree = ViewSpec::from_doc(&merge_docs(
            "views",
            &[LayerDoc::builtin(
                "views",
                include_str!("../../../../examples/demo-config/views.toml"),
            )
            .unwrap()],
        ))
        .0
        .into_iter()
        .find(|v| v.name == "tree")
        .expect("the demo tree view");
        let dims = DerivedDimensions::default();
        let ungrouped: Vec<String> = tree
            .ungrouped_dimensions(&schema, &dims)
            .iter()
            .map(|u| u.name.to_string())
            .collect();
        tree.columns
            .retain(|c| !ungrouped.iter().any(|u| u == c.name()));
        assert!(tree.ungrouped_dimensions(&schema, &dims).is_empty());

        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        for ds in &schema.datasets {
            store.apply_schema(ds).unwrap();
        }
        let q = compile_view(
            store.writer(),
            &tree,
            &schema,
            &Scope::default(),
            &dims,
            &crate::query::as_of::AsOf::Live,
            usize::MAX,
        )
        .unwrap();
        let expected = include_str!("testdata/demo_tree_view.sql");
        assert_eq!(format!("{}\n", q.sql), expected);
    }
}
