//! Scope to SQL (spec §6.2). Three predicate kinds composed with AND,
//! every value bound rather than spliced.
//!
//! Dimension selections bind one delimiter-joined varchar and split it in
//! SQL. duckdb-rs cannot bind a list parameter — `Value::List` binding is
//! an explicit error, verified against 1.10505. A connection-local temp
//! table was the first design and does not work here: compilation happens
//! on the service's connection and execution on a pool worker's, and temp
//! tables are connection-local. `string_split` keeps the statement text
//! stable regardless of selection size, so the prepared plan stays
//! cacheable, which was the temp table's other reason for existing.
//!
//! A predicate naming a column this grain's table does not carry is
//! evaluated against the grain that does, as a membership test on the
//! keys the two grains share. When those keys do not pin the other
//! grain's entity the result is marked `SemiJoined`: "positions that have
//! SPX risk" is not "the SPX share of the position" (spec §6.3). When they
//! do — an instrument attribute tested from underlying grain — the
//! predicate is functionally determined and stays `Direct`.

use crate::store::StoreError;
use crate::store::ddl::{TableKind, table_name};
use duckdb::Connection;
use duckdb::types::Value;
use geode_core::attribution::ScopeSemantics;
use geode_core::dimensions::DerivedDimensions;
use geode_core::schema::{DatasetSpec, Grain};
use geode_core::scope::{CompareOp, Expr, Literal, Scope};
use std::collections::HashMap;

/// Values are joined with this before binding and split back in SQL. A
/// control character no dimension value can contain.
const SELECTION_DELIMITER: &str = "\u{1f}";

#[derive(Debug, Clone)]
pub struct ScopeSql {
    /// A boolean expression, `true` when the scope is empty.
    pub predicate: String,
    /// Bound in order; the predicate carries `?` placeholders.
    pub params: Vec<Value>,
    pub semantics: ScopeSemantics,
}

/// Which tables a query reads and the generation filter that goes with
/// them (spec §6.5). Passed down so the semi-join probe reads the same
/// era as its caller — a probe left on live inside an as-of query mixes
/// today's data into a historical answer, and does it silently.
///
/// `generations` is the predicate `relation` applies itself, not a filter
/// callers apply afterward — every other site that used to `and` it onto
/// its own predicate has been deleted (Phase 4a's as-of baseline fix), so
/// `relation` is the sole applier **in every era**, `Live` included: a
/// `Live` era carrying `Some(predicate)` is filtered exactly like an
/// `Archive` era, even though no production path constructs one today
/// (`Era::live()` and `era_for`'s `AsOf::Live` arm both leave it `None`).
/// An arm that silently dropped a predicate it was handed would be a
/// trapdoor for the next caller who builds a `Live` era with one — this
/// type is `pub` with `pub` fields precisely so a future optimisation
/// (reading only `live` when every resolved generation is current) can
/// reach for it directly. Under `Archive`, both sides are always read,
/// filtered independently, because `publish_file` is one transaction *per
/// grain* while compile (the resolve) and execution run on separate
/// connections: a publish landing between them can move a generation from
/// live to archive at one grain and not another, so a relation that
/// trusted which side the resolve saw would silently miss that partition
/// for one frame.
#[derive(Clone, Copy)]
pub struct Era<'a> {
    pub kind: TableKind,
    pub generations: Option<&'a str>,
}

impl Era<'_> {
    pub fn live() -> Era<'static> {
        Era {
            kind: TableKind::Live,
            generations: None,
        }
    }

    /// The relation a query reads for one grain under this era.
    ///
    /// Live reads the live table and nothing else — filtered by
    /// `generations` too, if the caller set it, exactly like the archive
    /// side; today's callers never do (`Era::live()` and `era_for`'s
    /// `AsOf::Live` arm both leave it `None`), but `relation` is the sole
    /// applier of the predicate now that every caller-side application has
    /// been deleted (Phase 4a's as-of baseline fix), so an era carrying a
    /// predicate must never have it silently dropped by whichever arm
    /// happens to run. As-of reads the archive **and** live: the
    /// generation a partition holds *now* is in live and nowhere else, so
    /// a query as of any moment after that generation was published —
    /// including "as of an hour ago" for a book that refreshed this
    /// morning — has to find it there. Reading the archive alone answers
    /// such a query with the partition's *previous* generation, or with
    /// nothing at all for a partition published only once, and says
    /// nothing either way. The generation predicate — applied to *both*
    /// sides here, not by the caller — is what keeps the two sides from
    /// both contributing; live carries `gen_id` and `source_time`
    /// precisely so it can be filtered the same way (§4.2).
    pub fn relation(&self, dataset: &str, grain: Grain) -> String {
        match self.kind {
            TableKind::Live => match self.generations {
                Some(p) => format!(
                    "(select * from {} where {p})",
                    table_name(dataset, grain, TableKind::Live)
                ),
                None => table_name(dataset, grain, TableKind::Live),
            },
            TableKind::Archive => {
                let p = self.generations.unwrap_or("true");
                format!(
                    "(select * from {} where {p} union all select * from {} where {p})",
                    table_name(dataset, grain, TableKind::Archive),
                    table_name(dataset, grain, TableKind::Live)
                )
            }
        }
    }
}

/// Whether `column` can be evaluated on `grain`'s own rows: a dimension
/// key it carries, a carried dimension it carries (spec §3.3), or a
/// measure or attribute declared at it.
fn evaluable_at(ds: &DatasetSpec, dims: &DerivedDimensions, grain: Grain, column: &str) -> bool {
    let base = dims.base_column(column);
    ds.carries(grain, base) || ds.column(base).and_then(|c| c.grain()) == Some(grain)
}

/// Where a clause over `columns` is evaluated when compiling at `grain`.
///
/// `Ok(None)` means on this grain's own rows. `Ok(Some(g))` names another
/// declared grain that carries every column, to be reached by a
/// membership test; the coarsest such grain is chosen because it is the
/// smallest table. A clause no single grain can evaluate is an error at
/// compile time rather than a binder error inside the pool: the caller
/// can split it into top-level `and` terms, each of which routes alone.
fn route(
    ds: &DatasetSpec,
    dims: &DerivedDimensions,
    grain: Grain,
    columns: &[&str],
) -> Result<Option<Grain>, StoreError> {
    let unknown = |column: &str, why: &str| StoreError::Sql {
        statement: format!("scope predicate on '{column}'"),
        source: duckdb::Error::InvalidParameterName(format!(
            "'{column}' {why} in dataset '{}'",
            ds.name
        )),
    };
    for c in columns {
        let base = dims.base_column(c);
        let carried = Grain::ALL.iter().any(|g| ds.carries(*g, base));
        match ds.column(base) {
            None if !carried => return Err(unknown(c, "is not a column")),
            Some(col) if col.grain().is_none() && !carried => {
                return Err(unknown(
                    c,
                    "is not carried as a dimension by any grain, so it cannot be scoped",
                ));
            }
            _ => {}
        }
    }
    if columns.iter().all(|c| evaluable_at(ds, dims, grain, c)) {
        return Ok(None);
    }
    ds.grains()
        .into_iter()
        .find(|g| *g != grain && columns.iter().all(|c| evaluable_at(ds, dims, *g, c)))
        .map(Some)
        .ok_or_else(|| StoreError::Sql {
            statement: format!("scope predicate on {columns:?}"),
            source: duckdb::Error::InvalidParameterName(format!(
                "no single grain of dataset '{}' carries every column in {columns:?}; \
                 write predicates on columns of different grains as separate \
                 top-level `and` terms",
                ds.name
            )),
        })
}

/// The keys `grain` and `probe` share — the coarser one's dimension keys.
fn shared_keys(grain: Grain, probe: Grain) -> Vec<&'static str> {
    grain
        .dimension_key_columns()
        .iter()
        .copied()
        .filter(|k| probe.dimension_key_columns().contains(k))
        .collect()
}

/// Whether reaching `probe` from `grain` is a membership test rather than
/// a lookup: the shared keys do not pin the probe grain's entity, so the
/// predicate says "has a row that…" rather than selecting the row itself.
fn is_membership(grain: Grain, probe: Grain) -> bool {
    let keys = shared_keys(grain, probe);
    !probe.identity_columns().iter().all(|id| keys.contains(id))
}

/// `exists (…)` testing `inner` against `probe`'s rows for the same keys.
///
/// `is not distinct from`, not `=`: the key columns are matching rows of
/// the *same* entity, so a NULL here is a real value on both sides rather
/// than a rolled-up placeholder. Plain equality would make a position with
/// no LHU fail its own semi-join, and the row would still be present
/// carrying a coarse measure of NULL — visibly inconsistent rather than
/// merely absent.
///
/// The probe reads the same era as its caller — via `era.relation`, which
/// applies the generation predicate to both sides itself (Phase 4a's as-of
/// baseline fix). Reading live from inside an as-of query mixes today's
/// data into a historical answer — and does it silently, because the
/// numbers still look like numbers (spec §6.5).
fn membership(ds: &DatasetSpec, grain: Grain, probe: Grain, era: Era<'_>, inner: &str) -> String {
    let join = shared_keys(grain, probe)
        .iter()
        .map(|k| format!("probe.\"{k}\" is not distinct from base.\"{k}\""))
        .collect::<Vec<_>>()
        .join(" and ");
    let terms = [join, inner.to_string()];
    format!(
        "exists (select 1 from {} probe where {})",
        era.relation(&ds.name, probe),
        terms.join(" and ")
    )
}

/// `%text%` with LIKE's own wildcards escaped, so a trader typing `50_`
/// or `100%` searches for those characters rather than for anything.
/// Paired with `escape '\'` in the predicate.
fn like_pattern(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('%');
    for ch in text.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('%');
    out
}

/// The values of ENUM type `enum_type` matching `pattern` (already
/// produced by [`like_pattern`]), resolved once on `conn` at compile
/// time (spec §3.5, the literal-list form).
///
/// This is a small query — a dictionary is hundreds of values, not a
/// million rows — run synchronously on the compile connection, which is
/// safe because `compile_scope` already runs on the service thread
/// ahead of `submit`, not on a pool worker mid-query. Reached through
/// [`DictionaryCache`], which resolves it (and [`existing_enum_types`])
/// once per (type, pattern) — respectively per dataset — per statement,
/// not once per grain: `compile_view` calls `compile_scope` once per
/// measure grain plus once for the spine, and without the cache every
/// one of those repeated the same catalog round-trips.
///
/// [`existing_enum_types`]: crate::store::ddl::existing_enum_types
fn dictionary_matches(
    conn: &Connection,
    enum_type: &str,
    pattern: &str,
) -> Result<Vec<String>, StoreError> {
    let sql = format!(
        "select v from unnest(enum_range(null::{enum_type})) t(v) where v ilike ? escape '\\'"
    );
    let err = |source| StoreError::Sql {
        statement: sql.clone(),
        source,
    };
    let mut stmt = conn.prepare(&sql).map_err(err)?;
    let rows = stmt
        .query_map(duckdb::params![pattern], |r| r.get::<_, String>(0))
        .map_err(err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(err)
}

/// Catalog facts the text filter needs, resolved once per statement
/// rather than once per grain (`compile_view` calls `compile_scope`
/// once per measure grain plus once for the spine).
///
/// A fresh, empty cache is exactly as correct as no cache at all — it
/// just misses every lookup once — so [`compile_scope`] keeps working
/// unchanged as a thin wrapper handing [`compile_scope_cached`] a
/// throwaway `DictionaryCache::default()`.
///
/// **Never hold one across statements.** `refresh_enum` rebuilds every
/// ENUM type on every publish, so a `DictionaryCache` parked on
/// something longer-lived than a single compile — a `DataService`
/// field, a pool worker, a `static` — would keep answering with
/// whatever dictionary existed when it was first warmed. A book that
/// arrived in the newest generation would be silently absent from a
/// cached `matches` result, and a retired book would keep being bound
/// as though it were still live: wrong data with no error. Construct
/// one fresh per statement, as every call site in this crate does, and
/// let it drop at the end of that call.
///
/// `pub(crate)`, not exported from `query/mod.rs`: nothing outside this
/// crate compiles several grains of one statement, so nothing outside
/// it needs to hold a cache across calls (`compile_scope`'s own
/// unchanged public signature is the seam every other crate uses).
#[derive(Default)]
pub(crate) struct DictionaryCache {
    /// dataset name -> its existing ENUM type names.
    enum_types: HashMap<String, Vec<String>>,
    /// ENUM type -> LIKE pattern -> the dictionary values that matched.
    /// Nested rather than a single `(type, pattern)`-keyed map so a hit
    /// (`get(enum_type).and_then(|m| m.get(pattern))`) allocates nothing
    /// — only a miss needs to own the two strings to insert them. Two
    /// levels, not the type alone: two needles narrowing to different
    /// matches over the same column must not collide.
    matches: HashMap<String, HashMap<String, Vec<String>>>,
    /// Catalog round-trips actually made. Tests assert on it; the hot
    /// path never reads it.
    pub(crate) lookups: usize,
}

impl DictionaryCache {
    /// `existing_enum_types(conn, dataset)`, cached for the life of this
    /// `DictionaryCache`.
    pub(crate) fn enum_types(
        &mut self,
        conn: &Connection,
        dataset: &str,
    ) -> Result<&[String], StoreError> {
        if !self.enum_types.contains_key(dataset) {
            let v = crate::store::ddl::existing_enum_types(conn, dataset)?;
            self.lookups += 1;
            self.enum_types.insert(dataset.to_string(), v);
        }
        Ok(self
            .enum_types
            .get(dataset)
            .expect("just inserted this key"))
    }

    /// `dictionary_matches(conn, enum_type, pattern)`, cached for the
    /// life of this `DictionaryCache`.
    pub(crate) fn matches(
        &mut self,
        conn: &Connection,
        enum_type: &str,
        pattern: &str,
    ) -> Result<&[String], StoreError> {
        if self
            .matches
            .get(enum_type)
            .and_then(|m| m.get(pattern))
            .is_none()
        {
            let v = dictionary_matches(conn, enum_type, pattern)?;
            self.lookups += 1;
            self.matches
                .entry(enum_type.to_string())
                .or_default()
                .insert(pattern.to_string(), v);
        }
        Ok(self
            .matches
            .get(enum_type)
            .and_then(|m| m.get(pattern))
            .expect("just inserted this key"))
    }
}

/// One dimension selection as a predicate on the column it really reads,
/// with its values bound as a single delimiter-joined varchar and split
/// back by `string_split` in SQL — so the statement text, and therefore
/// the prepared plan, is the same whatever the selection's size.
///
/// `None` means "this selection can match nothing": a selection on a
/// derived dimension names *derived* values while the stored column holds
/// source ones, so it has to be translated back through the map, and a
/// derived value the map does not produce leaves no source value at all.
/// Binding the derived value against the source column would compile
/// cleanly and silently match nothing, which is the worst way for this to
/// fail (spec §6.8) — the caller turns `None` into an explicit "selects
/// nothing" instead.
///
/// `pub(crate)` because `compile_distinct`'s document arm binds the very
/// same selections without a grain to route them through (a document
/// dataset has none, market-data spec §3.3): the derived translation and
/// the binding form are spelled here once so the two paths cannot drift.
pub(crate) fn selection_clause(
    sel: &geode_core::scope::DimensionSelection,
    dims: &DerivedDimensions,
) -> Option<(String, Vec<Value>)> {
    let base = dims.base_column(&sel.column).to_string();
    let values: Vec<String> = match dims.get(&sel.column) {
        None => sel.values.clone(),
        Some(d) => d
            .values
            .iter()
            .filter(|(_, derived)| sel.values.contains(derived))
            .map(|(source, _)| source.clone())
            .collect(),
    };
    if values.is_empty() {
        return None;
    }
    Some((
        format!("\"{base}\" in (select unnest(string_split(?, '{SELECTION_DELIMITER}')))"),
        vec![Value::Text(values.join(SELECTION_DELIMITER))],
    ))
}

/// Top-level `and` terms, each routed on its own so a scope mixing
/// grains — `underlying_ref = 'SPX' and strike > 100` — compiles.
fn conjuncts(expr: &Expr) -> Vec<&Expr> {
    match expr {
        Expr::And(a, b) => {
            let mut out = conjuncts(a);
            out.extend(conjuncts(b));
            out
        }
        other => vec![other],
    }
}

/// Compile the scope for the rows of `grain`, under `era`.
///
/// A thin wrapper over [`compile_scope_cached`] with a throwaway,
/// call-local cache: every one of the twenty-odd existing callers keeps
/// this exact signature, and a cache that lives no longer than one call
/// is exactly as correct as no cache — it just costs one miss instead of
/// none. A caller compiling several grains of the same statement (that
/// is: `compile_view`, `compile_distinct`) should hold its own
/// `DictionaryCache` across those calls instead, via
/// `compile_scope_cached`.
pub fn compile_scope(
    // Used by the text filter (spec §3.5): whether a categorical column's
    // ENUM type exists is a catalog lookup, not something the scope's own
    // predicate can know.
    conn: &Connection,
    scope: &Scope,
    ds: &DatasetSpec,
    grain: Grain,
    dims: &DerivedDimensions,
    era: Era<'_>,
) -> Result<ScopeSql, StoreError> {
    compile_scope_cached(
        conn,
        scope,
        ds,
        grain,
        dims,
        era,
        &mut DictionaryCache::default(),
    )
}

/// [`compile_scope`], but resolving the text filter's catalog facts
/// (ENUM type existence, dictionary matches) through a `DictionaryCache`
/// the caller supplies — so a caller compiling several grains of one
/// statement resolves each dataset's ENUM types and each (type, needle)
/// pattern's matches once, not once per grain. `pub(crate)`, like
/// `DictionaryCache` itself — see its doc comment for why.
pub(crate) fn compile_scope_cached(
    conn: &Connection,
    scope: &Scope,
    ds: &DatasetSpec,
    grain: Grain,
    dims: &DerivedDimensions,
    era: Era<'_>,
    cache: &mut DictionaryCache,
) -> Result<ScopeSql, StoreError> {
    let nothing = || ScopeSql {
        predicate: "false".to_string(),
        params: Vec::new(),
        semantics: ScopeSemantics::Direct,
    };
    // A contradiction selects nothing, and must say so in SQL. Returning
    // early matters: the contradicted dimension has already been dropped
    // from `dimensions`, so compiling the rest would produce a predicate
    // that is *wider* than either layer asked for (see `Scope::and_then`).
    if scope.impossible {
        return Ok(nothing());
    }

    // Each clause carries its own bound values.
    //
    // Accumulating a flat `params` alongside the clause strings is what
    // made this silently wrong before: params were pushed in *source*
    // order, but the `exists(...)` wrapper holding every finer clause is
    // emitted *last*, so a finer predicate followed by a direct one
    // transposed their values onto each other's placeholders — a book
    // filter and an underlying filter swapping, with no error. Keeping the
    // values attached to the clause means the two orders cannot disagree:
    // the params fall out of the emission order rather than being
    // maintained in parallel with it.
    type Clause = (String, Vec<Value>);
    struct Routing {
        /// Clauses evaluated on this grain's own rows, in source order.
        direct: Vec<Clause>,
        /// Clauses reached through another grain, in source order;
        /// grouped by grain at emission so each probe is one `exists`.
        probed: Vec<(Grain, Clause)>,
        semi_dimensions: Vec<String>,
    }
    // Route one clause. The dimensions applied by membership are the
    // columns this grain does not itself carry.
    fn place(
        r: &mut Routing,
        ds: &DatasetSpec,
        dims: &DerivedDimensions,
        grain: Grain,
        columns: &[&str],
        clause: Clause,
    ) -> Result<(), StoreError> {
        match route(ds, dims, grain, columns)? {
            None => r.direct.push(clause),
            Some(probe) => {
                if is_membership(grain, probe) {
                    for c in columns {
                        let base = dims.base_column(c);
                        if !evaluable_at(ds, dims, grain, c)
                            && !r.semi_dimensions.iter().any(|s| s == base)
                        {
                            r.semi_dimensions.push(base.to_string());
                        }
                    }
                }
                r.probed.push((probe, clause));
            }
        }
        Ok(())
    }
    let mut r = Routing {
        direct: Vec::new(),
        probed: Vec::new(),
        semi_dimensions: Vec::new(),
    };

    // 1. Dimension selections: one bound varchar, split in SQL.
    for sel in &scope.dimensions {
        if sel.values.is_empty() {
            continue;
        }
        let Some(clause) = selection_clause(sel, dims) else {
            // Selected a derived value the map does not produce: nothing
            // can match, and saying so beats an empty `in ()`.
            return Ok(nothing());
        };
        place(&mut r, ds, dims, grain, &[sel.column.as_str()], clause)?;
    }

    // 2. Text filter: OR of a literal-list `IN` over each categorical
    // textual column's matching dictionary values, plus a plain `ILIKE`
    // over any textual column that is not categorical (spec §3.5, the
    // literal-list form).
    //
    // Each column is routed on its own, because the OR cannot be split:
    // a textual column this grain does not carry becomes its own
    // membership term inside the OR, and the whole filter is one direct
    // clause. Leaving such columns out — the earlier choice — silently
    // applied the filter to the fine-grained measures and not to the
    // coarse ones on the same row, and marked nothing (spec §6.3).
    //
    // The literal-list form replaced a subquery form
    // (`"col" in (select v from unnest(enum_range(...)) t(v) where v
    // ilike ? escape '\')`) that still cost DuckDB a real per-requery
    // planning-and-probing bill even for a needle that matches nothing:
    // an OR of several such correlated subqueries measured ~65ms on a
    // wide demo schema at 1M rows before any row of the result was ever
    // touched (`docs/perf.md`, "literal-list form"). Resolving each
    // column's matches once here, on the compile connection
    // (`compile_scope` already runs on the service thread ahead of
    // `submit`, so this synchronous query is safe), turns that into a
    // handful of `?`-bound values known before the statement is ever
    // planned: a column with no matches drops its term entirely instead
    // of compiling to an always-false subquery, and the matching values
    // are bound the same way a dimension selection is — one
    // delimiter-joined varchar split by `string_split` in SQL — so the
    // statement text (and therefore the prepared plan) stays independent
    // of match count and cacheable regardless of how many values match.
    //
    // If every column's dictionary drops the needle — or the dataset
    // declares no textual columns at all — the OR would otherwise become
    // empty and contribute no clause, silently widening the scope to
    // "everything" instead of "nothing": rule enforced below by pushing
    // a literal `false` clause when no term survives.
    if let Some(text) = &scope.text {
        let pattern_text = like_pattern(text);
        let pattern = Value::Text(pattern_text.clone());
        // Type existence is the only gate (spec §3.5, as amended): it
        // holds in every era, because `refresh_enum` builds the type
        // from live *and* archive (`crate::store::ddl`), so an archived
        // row can never hold a value the type lacks.
        //
        // Cloned into an owned `Vec` rather than held as the cache's own
        // borrow: `cache.matches` below also needs `&mut cache` inside
        // this same loop, and a borrow of `enum_types` alive across every
        // iteration would conflict with it. The list is short (a
        // dataset's categorical textual columns), so the clone is cheap.
        let enum_types: Vec<String> = cache.enum_types(conn, &ds.name)?.to_vec();
        let mut terms: Vec<String> = Vec::new();
        let mut term_params: Vec<Value> = Vec::new();
        for col in ds.textual_columns() {
            let name = col.name.as_str();
            let ty = crate::store::ddl::enum_type_name(&ds.name, name);
            let bound;
            let test = if col.categorical && enum_types.contains(&ty) {
                let matches = cache.matches(conn, &ty, &pattern_text)?;
                if matches.is_empty() {
                    // No dictionary value meets the needle: this column
                    // contributes nothing, rather than an always-false
                    // subquery DuckDB would still have to plan.
                    continue;
                }
                bound = Value::Text(matches.join(SELECTION_DELIMITER));
                format!("\"{name}\" in (select unnest(string_split(?, '{SELECTION_DELIMITER}')))")
            } else {
                bound = pattern.clone();
                format!("\"{name}\" ilike ? escape '\\'")
            };
            match route(ds, dims, grain, &[name])? {
                None => terms.push(test),
                Some(probe) => {
                    if is_membership(grain, probe) && !r.semi_dimensions.iter().any(|s| s == name) {
                        r.semi_dimensions.push(name.to_string());
                    }
                    terms.push(membership(ds, grain, probe, era, &test));
                }
            }
            term_params.push(bound);
        }
        if terms.is_empty() {
            // Every column dropped, or there were none to begin with: a
            // text filter that matches nothing must select nothing,
            // never fall through to contributing no clause at all.
            r.direct.push(("false".to_string(), Vec::new()));
        } else {
            r.direct
                .push((format!("({})", terms.join(" or ")), term_params));
        }
    }

    // 3. Expression filter: AST lowered, literals bound, one clause per
    // top-level conjunct so each can be routed to the grain that carries
    // its columns.
    if let Some(expr) = &scope.expression {
        for term in conjuncts(expr) {
            let mut expr_params = Vec::new();
            let rendered = render_expr(term, &mut expr_params, dims)?;
            let columns = term.columns();
            place(&mut r, ds, dims, grain, &columns, (rendered, expr_params))?;
        }
    }

    // Probed clauses: one `exists` per grain, holding every clause routed
    // there. The wrappers are appended after the direct clauses, so their
    // values follow every direct clause's — which is exactly what carrying
    // the values with the clause gives.
    let Routing {
        mut direct,
        probed,
        semi_dimensions,
    } = r;
    for probe in ds.grains() {
        let mine: Vec<&Clause> = probed
            .iter()
            .filter(|(g, _)| *g == probe)
            .map(|(_, c)| c)
            .collect();
        if mine.is_empty() {
            continue;
        }
        let inner = mine
            .iter()
            .map(|(clause, _)| clause.clone())
            .collect::<Vec<_>>()
            .join(" and ");
        let inner_params: Vec<Value> = mine.iter().flat_map(|(_, p)| p.clone()).collect();
        direct.push((membership(ds, grain, probe, era, &inner), inner_params));
    }

    // One pass: the predicate and its values come out of the same
    // iteration, so they cannot be in different orders.
    let mut params: Vec<Value> = Vec::new();
    let mut clauses: Vec<String> = Vec::new();
    for (clause, clause_params) in direct {
        clauses.push(clause);
        params.extend(clause_params);
    }
    let predicate = if clauses.is_empty() {
        "true".to_string()
    } else {
        clauses.join(" and ")
    };

    let semantics = if semi_dimensions.is_empty() {
        ScopeSemantics::Direct
    } else {
        ScopeSemantics::SemiJoined {
            dimensions: semi_dimensions,
        }
    };

    Ok(ScopeSql {
        predicate,
        params,
        semantics,
    })
}

/// The source values a derived dimension maps to `wanted`, as bound
/// parameters against its source column.
///
/// The stored column holds source values, so an expression naming a
/// derived dimension has to be translated the same way a selection is —
/// otherwise it binds a derived value against a source column and matches
/// nothing, or names a column no table has.
fn derived_membership(
    d: &geode_core::dimensions::DerivedDimension,
    wanted: &[&Literal],
    negated: bool,
    params: &mut Vec<Value>,
) -> String {
    let sources: Vec<&String> = d
        .values
        .iter()
        .filter(|(_, derived)| {
            wanted
                .iter()
                .any(|w| matches!(w, Literal::Str(s) if s == *derived))
        })
        .map(|(source, _)| source)
        .collect();
    if sources.is_empty() {
        // No source value produces the requested derived value, so the
        // predicate is a constant — and saying which constant beats
        // emitting an empty `in ()`.
        return if negated { "true" } else { "false" }.to_string();
    }
    let marks = sources
        .iter()
        .map(|s| {
            params.push(Value::Text((*s).clone()));
            "?"
        })
        .collect::<Vec<_>>()
        .join(", ");
    let not = if negated { "not " } else { "" };
    format!("\"{}\" {not}in ({marks})", d.from)
}

/// Lower a validated expression, pushing every literal onto `params`.
///
/// `dims` is threaded through so a derived dimension is resolved to its
/// source column here too, not only in dimension selections (spec §6.8).
fn render_expr(
    expr: &Expr,
    params: &mut Vec<Value>,
    dims: &DerivedDimensions,
) -> Result<String, StoreError> {
    let unsupported = |column: &str, op: &str| StoreError::Sql {
        statement: format!("scope expression on derived dimension '{column}'"),
        source: duckdb::Error::InvalidParameterName(format!(
            "'{column}' is a derived dimension, so '{op}' has no meaning on it; \
             use = , != or in"
        )),
    };
    Ok(match expr {
        Expr::And(a, b) => format!(
            "({} and {})",
            render_expr(a, params, dims)?,
            render_expr(b, params, dims)?
        ),
        Expr::Or(a, b) => format!(
            "({} or {})",
            render_expr(a, params, dims)?,
            render_expr(b, params, dims)?
        ),
        Expr::Not(e) => format!("(not {})", render_expr(e, params, dims)?),
        Expr::Compare { column, op, value } => match dims.get(column) {
            // Equality is the only ordering-free comparison, and a
            // derived dimension has no order of its own — `desk > 'EU'`
            // would compare whatever the map happens to spell.
            Some(d) => match op {
                CompareOp::Eq => derived_membership(d, &[value], false, params),
                CompareOp::Ne => derived_membership(d, &[value], true, params),
                other => return Err(unsupported(column, other.sql())),
            },
            None => {
                params.push(literal_value(value));
                format!("\"{column}\" {} ?", op.sql())
            }
        },
        Expr::In { column, values } => match dims.get(column) {
            Some(d) => {
                let wanted: Vec<&Literal> = values.iter().collect();
                derived_membership(d, &wanted, false, params)
            }
            None => {
                let marks = values
                    .iter()
                    .map(|v| {
                        params.push(literal_value(v));
                        "?"
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("\"{column}\" in ({marks})")
            }
        },
    })
}

fn literal_value(l: &Literal) -> Value {
    match l {
        Literal::Str(s) => Value::Text(s.clone()),
        Literal::Num(n) => Value::Double(*n),
        Literal::Bool(b) => Value::Boolean(*b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::attribution::ScopeSemantics;
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::dimensions::DerivedDimensions;
    use geode_core::schema::{Grain, SchemaSpec};
    use geode_core::scope::{DimensionSelection, Scope, parse_expr};
    use proptest::prelude::*;

    fn dataset() -> geode_core::schema::DatasetSpec {
        let text = r#"
[risk.columns.book]
type = "utf8"
role = "dimension"
textual = true
[risk.columns.lhu]
type = "utf8"
role = "dimension"
[risk.columns.position_ref]
type = "utf8"
role = "key"
[risk.columns.counterparty]
type = "utf8"
role = "dimension"
[risk.columns.instrument_ref]
type = "utf8"
role = "key"
[risk.columns.underlying_ref]
type = "utf8"
role = "dimension"
textual = true
[risk.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
[risk.columns.daily_trading_pnl]
type = "f64"
role = "measure"
grain = "position"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc)
            .0
            .dataset("risk")
            .unwrap()
            .clone()
    }

    fn dims() -> DerivedDimensions {
        DerivedDimensions::default()
    }

    /// The Phase 4 §3.3 fixture: `currency` carried by the instrument
    /// grain. Both position (`daily_trading_pnl`) and instrument (`npv`)
    /// measures are declared so `ds.grains()` includes both, which is
    /// what lets `route` probe from position to instrument.
    fn carried_dataset() -> geode_core::schema::DatasetSpec {
        let text = r#"
[risk.columns.book]
type = "utf8"
role = "dimension"
[risk.columns.lhu]
type = "utf8"
role = "dimension"
[risk.columns.position_ref]
type = "utf8"
role = "key"
[risk.columns.counterparty]
type = "utf8"
role = "dimension"
[risk.columns.instrument_ref]
type = "utf8"
role = "key"
[risk.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk.columns.currency]
type = "utf8"
role = "dimension"
grain = "instrument"
[risk.columns.npv]
type = "f64"
role = "measure"
grain = "instrument"
[risk.columns.daily_trading_pnl]
type = "f64"
role = "measure"
grain = "position"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc)
            .0
            .dataset("risk")
            .unwrap()
            .clone()
    }

    fn store() -> (tempfile::TempDir, crate::store::Store) {
        let d = tempfile::tempdir().unwrap();
        let s = crate::store::Store::open(d.path().join("g.duckdb")).unwrap();
        (d, s)
    }

    fn compile(scope: &Scope, grain: Grain) -> (ScopeSql, tempfile::TempDir, crate::store::Store) {
        let (dir, store) = store();
        let sql = compile_scope(
            store.writer(),
            scope,
            &dataset(),
            grain,
            &dims(),
            Era::live(),
        )
        .unwrap();
        (sql, dir, store)
    }

    #[test]
    fn an_empty_scope_compiles_to_a_true_predicate() {
        let (sql, _d, _s) = compile(&Scope::default(), Grain::Underlying);
        assert_eq!(sql.predicate, "true");
        assert!(sql.params.is_empty());
        assert_eq!(sql.semantics, ScopeSemantics::Direct);
    }

    #[test]
    fn an_archive_era_relation_filters_both_sides() {
        // Phase 4a's as-of baseline fix: the generation predicate is
        // applied inside `relation` itself, to both the archive and live
        // tables it reads — not by the caller afterward. Both sides are
        // always read because publish is per grain and the resolve is a
        // separate statement (see `Era`'s doc comment), so both must be
        // filtered or one of them leaks an unresolved generation.
        let era = Era {
            kind: TableKind::Archive,
            generations: Some("gen_id = 7"),
        };
        let sql = era.relation("risk", Grain::Underlying);
        assert_eq!(
            sql.matches("gen_id = 7").count(),
            2,
            "the predicate must appear once per side: {sql}"
        );
        assert!(
            sql.contains("risk_underlying_archive where gen_id = 7"),
            "{sql}"
        );
        assert!(
            sql.contains("risk_underlying_live where gen_id = 7"),
            "{sql}"
        );
    }

    #[test]
    fn a_live_era_relation_honours_a_generation_predicate() {
        // `relation` is the sole applier of the generation predicate in
        // every era now that every caller-side application has been
        // deleted (Phase 4a's as-of baseline fix) — including `Live`,
        // which no production path hands a predicate today but which
        // must not silently drop one it is given. A `Live` arm that
        // ignored `generations` would be a trapdoor for the next
        // optimisation that reads only `live` when every resolved
        // generation is current.
        let era = Era {
            kind: TableKind::Live,
            generations: Some("gen_id = 7"),
        };
        let sql = era.relation("risk", Grain::Underlying);
        assert!(
            sql.contains("risk_underlying_live where gen_id = 7"),
            "a Live era carrying a predicate must be filtered by it: {sql}"
        );
    }

    #[test]
    fn a_dimension_selection_binds_one_value_and_splits_it_in_sql() {
        // duckdb-rs cannot bind a list, and a temp table would be
        // connection-local — compilation and execution happen on
        // different connections (spec §6.2).
        let scope = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into(), "BK001".into()],
            }],
            ..Scope::default()
        };
        let (sql, _d, _s) = compile(&scope, Grain::Underlying);
        assert!(sql.predicate.contains("string_split"), "{}", sql.predicate);
        assert_eq!(sql.params.len(), 1, "one bound value, not one per book");
    }

    #[test]
    fn the_statement_text_does_not_grow_with_the_selection() {
        let small = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into()],
            }],
            ..Scope::default()
        };
        let large = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: (0..500).map(|i| format!("BK{i:03}")).collect(),
            }],
            ..Scope::default()
        };
        // One bound varchar either way, so the two statements must be
        // character-identical — that is what keeps the plan cacheable.
        let (dir, store) = store();
        let a = compile_scope(
            store.writer(),
            &small,
            &dataset(),
            Grain::Underlying,
            &dims(),
            Era::live(),
        )
        .unwrap();
        let b = compile_scope(
            store.writer(),
            &large,
            &dataset(),
            Grain::Underlying,
            &dims(),
            Era::live(),
        )
        .unwrap();
        assert_eq!(
            a.predicate, b.predicate,
            "a cacheable plan requires stable text"
        );
        assert_eq!(a.params.len(), b.params.len(), "one param either way");
        drop(dir);
    }

    #[test]
    fn the_text_filter_ors_ilike_over_declared_textual_columns_only() {
        let scope = Scope {
            text: Some("SPX".into()),
            ..Scope::default()
        };
        let (sql, _d, _s) = compile(&scope, Grain::Underlying);
        assert!(sql.predicate.contains("ilike"), "{}", sql.predicate);
        assert!(sql.predicate.contains("\"book\""), "{}", sql.predicate);
        assert!(
            sql.predicate.contains("\"underlying_ref\""),
            "{}",
            sql.predicate
        );
        // lhu is not declared textual.
        assert!(!sql.predicate.contains("\"lhu\""), "{}", sql.predicate);
        assert_eq!(sql.params.len(), 2, "one bound pattern per textual column");
    }

    #[test]
    fn the_text_filter_escapes_likes_own_wildcards() {
        // `50_` must find `50_` and not `500`; `%` must find `%`.
        let (dir, store) = store();
        store
            .writer()
            .execute_batch(
                "create table risk_underlying_live(
                     book varchar, lhu varchar, position_ref varchar,
                     counterparty varchar, instrument_ref varchar,
                     underlying_ref varchar, delta01 double,
                     batch varchar, source_file_id bigint,
                     gen_id bigint, source_time timestamp with time zone);
                 insert into risk_underlying_live values
                   ('BK50_','L','P1','C','I1','SPX', 1, 'b', 1, 1, now()),
                   ('BK500','L','P2','C','I2','RUT', 10, 'b', 1, 1, now()),
                   ('BK%','L','P3','C','I3','NDX', 100, 'b', 1, 1, now());",
            )
            .unwrap();
        let total = |text: &str| -> f64 {
            let sql = compile_scope(
                store.writer(),
                &Scope {
                    text: Some(text.into()),
                    ..Scope::default()
                },
                &dataset(),
                Grain::Underlying,
                &dims(),
                Era::live(),
            )
            .unwrap();
            store
                .writer()
                .query_row(
                    &format!(
                        "select coalesce(sum(delta01), 0) from risk_underlying_live where {}",
                        sql.predicate
                    ),
                    duckdb::params_from_iter(sql.params.iter()),
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(
            total("50_"),
            1.0,
            "an underscore is a character, not a wildcard"
        );
        assert_eq!(total("%"), 100.0, "so is a percent sign");
        assert_eq!(
            total("BK5"),
            11.0,
            "and an ordinary prefix still matches broadly"
        );
        drop(dir);
    }

    #[test]
    fn an_attribute_of_a_coarser_grain_is_a_lookup_not_a_membership_test() {
        // `strike` is an instrument attribute. From underlying grain the
        // shared keys include `instrument_ref`, which pins the instrument,
        // so the predicate is functionally determined: Direct, through a
        // probe of the instrument table. From position grain the keys do
        // not name the instrument, so it is "positions that hold an
        // instrument with…" — SemiJoined.
        let mut ds = dataset();
        ds.columns.push(geode_core::schema::ColumnSpec {
            name: "strike".into(),
            source_name: None,
            ty: geode_core::schema::ColumnType::F64,
            required: false,
            textual: false,
            categorical: false,
            role: geode_core::schema::ColumnRole::Attribute {
                grain: Some(Grain::Instrument),
            },
        });
        let (dir, store) = store();
        let scope = Scope {
            expression: Some(parse_expr("strike > 100").unwrap()),
            ..Scope::default()
        };
        let at = |grain: Grain| {
            compile_scope(store.writer(), &scope, &ds, grain, &dims(), Era::live()).unwrap()
        };
        let fine = at(Grain::Underlying);
        assert!(
            fine.predicate.contains("from risk_instrument_live probe"),
            "{}",
            fine.predicate
        );
        assert!(
            fine.predicate
                .contains("probe.\"instrument_ref\" is not distinct from"),
            "{}",
            fine.predicate
        );
        assert_eq!(fine.semantics, ScopeSemantics::Direct);

        let coarse = at(Grain::Position);
        assert!(
            !coarse.predicate.contains("instrument_ref"),
            "{}",
            coarse.predicate
        );
        assert_eq!(
            coarse.semantics,
            ScopeSemantics::SemiJoined {
                dimensions: vec!["strike".into()]
            }
        );

        let own = at(Grain::Instrument);
        assert!(!own.predicate.contains("exists"), "{}", own.predicate);
        drop(dir);
    }

    #[test]
    fn conjuncts_of_different_grains_route_separately() {
        // `underlying_ref = 'SPX' and book = 'BK000'` from position grain:
        // the first needs the underlying table, the second is on this one.
        // One clause per top-level `and`, each routed on its own.
        let (dir, store) = store();
        let scope = Scope {
            expression: Some(parse_expr("underlying_ref = 'SPX' and book = 'BK000'").unwrap()),
            ..Scope::default()
        };
        let sql = compile_scope(
            store.writer(),
            &scope,
            &dataset(),
            Grain::Position,
            &dims(),
            Era::live(),
        )
        .unwrap();
        assert_eq!(
            sql.predicate.matches("exists").count(),
            1,
            "{}",
            sql.predicate
        );
        assert!(
            sql.predicate.starts_with("\"book\" = ?"),
            "the direct term first: {}",
            sql.predicate
        );
        assert_eq!(sql.params.len(), 2);
        assert_eq!(
            sql.semantics,
            ScopeSemantics::SemiJoined {
                dimensions: vec!["underlying_ref".into()]
            }
        );

        // Inside one term the split is not possible, and an OR across
        // grains has no single table to evaluate on: a loud error at
        // compile time, not a binder error in the pool.
        let mixed = Scope {
            expression: Some(parse_expr("underlying_ref = 'SPX' or delta01 > 1").unwrap()),
            ..Scope::default()
        };
        let ok = compile_scope(
            store.writer(),
            &mixed,
            &dataset(),
            Grain::Position,
            &dims(),
            Era::live(),
        );
        assert!(ok.is_ok(), "both columns live on the underlying table");
        let unknown = Scope {
            expression: Some(parse_expr("nosuch = 1").unwrap()),
            ..Scope::default()
        };
        assert!(
            compile_scope(
                store.writer(),
                &unknown,
                &dataset(),
                Grain::Position,
                &dims(),
                Era::live(),
            )
            .is_err(),
            "an unknown column fails at compile time"
        );
        drop(dir);
    }

    #[test]
    fn expression_literals_are_bound_never_spliced() {
        let scope = Scope {
            expression: Some(parse_expr("book = 'BK000' and delta01 > 100").unwrap()),
            ..Scope::default()
        };
        let (sql, _d, _s) = compile(&scope, Grain::Underlying);
        assert!(!sql.predicate.contains("BK000"), "{}", sql.predicate);
        assert_eq!(sql.params.len(), 2);
        assert!(sql.predicate.contains('?'), "{}", sql.predicate);
    }

    #[test]
    fn a_finer_column_becomes_a_semi_join_at_a_coarser_grain() {
        // Scoping to an underlying while asking for a position measure:
        // "positions that have SPX risk" (spec §6.3).
        let scope = Scope {
            dimensions: vec![DimensionSelection {
                column: "underlying_ref".into(),
                values: vec!["SPX".into()],
            }],
            ..Scope::default()
        };
        let (sql, _d, _s) = compile(&scope, Grain::Position);
        assert!(sql.predicate.contains("exists"), "{}", sql.predicate);
        match &sql.semantics {
            ScopeSemantics::SemiJoined { dimensions } => {
                assert_eq!(dimensions, &["underlying_ref".to_string()]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_same_predicate_is_direct_at_its_own_grain() {
        let scope = Scope {
            dimensions: vec![DimensionSelection {
                column: "underlying_ref".into(),
                values: vec!["SPX".into()],
            }],
            ..Scope::default()
        };
        let (sql, _d, _s) = compile(&scope, Grain::Underlying);
        assert!(!sql.predicate.contains("exists"), "{}", sql.predicate);
        assert_eq!(sql.semantics, ScopeSemantics::Direct);
    }

    #[test]
    fn the_compiled_predicate_actually_filters() {
        // Compile then run it, so a predicate that is merely well-formed
        // but wrong cannot pass.
        let (dir, store) = store();
        store
            .writer()
            .execute_batch(
                "create table risk_snapshot_underlying_live(
                     book varchar, lhu varchar, position_ref varchar,
                     counterparty varchar, instrument_ref varchar,
                     underlying_ref varchar, delta01 double,
                     batch varchar, source_file_id bigint,
                     gen_id bigint, source_time timestamp with time zone);
                 insert into risk_snapshot_underlying_live values
                   ('BK000','L','P1','C','I1','SPX', 10, 'b', 1, 1, now()),
                   ('BK001','L','P2','C','I2','RUT', 20, 'b', 1, 1, now());",
            )
            .unwrap();

        let scope = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into()],
            }],
            ..Scope::default()
        };
        let sql = compile_scope(
            store.writer(),
            &scope,
            &dataset(),
            Grain::Underlying,
            &dims(),
            Era::live(),
        )
        .unwrap();
        let total: f64 = store
            .writer()
            .query_row(
                &format!(
                    "select sum(delta01) from risk_snapshot_underlying_live where {}",
                    sql.predicate
                ),
                duckdb::params_from_iter(sql.params.iter()),
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(total, 10.0, "only BK000's row survives");
        drop(dir);
    }

    #[test]
    fn a_selection_on_a_carried_dimension_is_direct_where_carried_and_probed_from_position() {
        let ds = carried_dataset();
        let (_dir, store) = store();
        let scope = Scope {
            dimensions: vec![DimensionSelection {
                column: "currency".into(),
                values: vec!["USD".into()],
            }],
            ..Scope::default()
        };
        let at_instrument = compile_scope(
            store.writer(),
            &scope,
            &ds,
            Grain::Instrument,
            &dims(),
            Era::live(),
        )
        .unwrap();
        assert!(at_instrument.predicate.contains("\"currency\" in"));
        assert!(
            !at_instrument.predicate.contains("exists"),
            "{}",
            at_instrument.predicate
        );
        assert_eq!(at_instrument.semantics, ScopeSemantics::Direct);

        let at_position = compile_scope(
            store.writer(),
            &scope,
            &ds,
            Grain::Position,
            &dims(),
            Era::live(),
        )
        .unwrap();
        assert!(
            at_position.predicate.contains("exists"),
            "{}",
            at_position.predicate
        );
        assert_eq!(
            at_position.semantics,
            ScopeSemantics::SemiJoined {
                dimensions: vec!["currency".into()]
            },
            "positions that have USD risk, not the USD share of the position"
        );
    }

    /// The Phase 4 §3.5 fixture: `book` is categorical and textual, and
    /// `risk_instrument_live` actually carries its ENUM type — twenty
    /// plain books, `BK000` through `BK019`, plus three whose *value*
    /// carries a LIKE special character (`BK_01`, `BK%02`, `BK\03`). The
    /// escape clause only matters for a needle that can meet one of
    /// those: none of BK000..BK019 does, which is why the first version
    /// of this fixture let a mutation dropping `escape '\'` survive —
    /// a fixture that cannot reach the defect, the class CLAUDE.md warns
    /// about. One instrument row each. The tempdir is deliberately
    /// leaked (not returned) so the fixture stays a two-tuple as every
    /// call site below expects; the file lives for the process lifetime,
    /// which a test run can afford.
    fn enum_fixture() -> (crate::store::Store, geode_core::schema::DatasetSpec) {
        let mut ds = carried_dataset();
        for c in ds.columns.iter_mut() {
            if c.name == "book" {
                c.categorical = true;
                c.textual = true;
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        std::mem::forget(dir);
        store.apply_schema(&ds).unwrap();
        let conn = store.writer();
        let insert = "insert into risk_instrument_live
                     (book, lhu, position_ref, counterparty, instrument_ref,
                      npv, currency, batch, source_file_id, gen_id, source_time)
                 values (?, 'L', ?, 'C', ?, 1.0, 'USD', 'b', 1, 1, now())";
        for i in 0..20 {
            conn.execute(
                insert,
                duckdb::params![format!("BK{i:03}"), format!("P{i}"), format!("I{i}")],
            )
            .unwrap();
        }
        for (n, book) in ["BK_01", "BK%02", "BK\\03"].into_iter().enumerate() {
            conn.execute(
                insert,
                duckdb::params![book, format!("PS{n}"), format!("IS{n}")],
            )
            .unwrap();
        }
        crate::store::ddl::refresh_enum(
            conn,
            "risk",
            "book",
            "risk_instrument_live",
            "risk_instrument_archive",
        )
        .unwrap();
        (store, ds)
    }

    /// Review round 1, Major 1: two categorical textual columns —
    /// `book` and `counterparty` — whose dictionaries each hold exactly
    /// one value matching the needle `"match"`, but a *different* value
    /// each. Every other text-filter fixture in this file declares
    /// exactly one categorical textual column, so a `DictionaryCache`
    /// key that dropped the ENUM type and collapsed to the pattern
    /// alone would have nothing to collide with and no test could see
    /// it — the exact "fixture that cannot reach the defect" class
    /// `CLAUDE.md` warns about.
    fn two_categorical_columns_fixture() -> (crate::store::Store, geode_core::schema::DatasetSpec) {
        let mut ds = carried_dataset();
        for c in ds.columns.iter_mut() {
            if c.name == "book" || c.name == "counterparty" {
                c.categorical = true;
                c.textual = true;
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        std::mem::forget(dir);
        store.apply_schema(&ds).unwrap();
        let conn = store.writer();
        let insert = "insert into risk_instrument_live
                     (book, lhu, position_ref, counterparty, instrument_ref,
                      npv, currency, batch, source_file_id, gen_id, source_time)
                 values (?, 'L', ?, ?, ?, 1.0, 'USD', 'b', 1, 1, now())";
        conn.execute(insert, duckdb::params!["BKMATCH", "P0", "CP_OTHER", "I0"])
            .unwrap();
        conn.execute(insert, duckdb::params!["BK_OTHER", "P1", "CPMATCH", "I1"])
            .unwrap();
        for col in ["book", "counterparty"] {
            crate::store::ddl::refresh_enum(
                conn,
                "risk",
                col,
                "risk_instrument_live",
                "risk_instrument_archive",
            )
            .unwrap();
        }
        (store, ds)
    }

    /// The as-of root-cause fixture (spec §3.5, as amended): `BK_OLD`
    /// exists ONLY in the archive — `enum_fixture`'s live table never
    /// carried it — so a dictionary built from live alone cannot hold
    /// it. This is the exact case the era gate's original comment
    /// worried about ("an archived row could hold a value absent from
    /// the dictionary and the `IN` would silently drop it"); the fix is
    /// `refresh_enum` reading live *and* archive, not leaving the
    /// rewrite gated off.
    fn archived_only_value_fixture() -> (crate::store::Store, geode_core::schema::DatasetSpec) {
        let (store, ds) = enum_fixture();
        let conn = store.writer();
        conn.execute(
            "insert into risk_instrument_archive
                 (book, lhu, position_ref, counterparty, instrument_ref,
                  npv, currency, batch, source_file_id, gen_id, source_time)
             values ('BK_OLD', 'L', 'POLD', 'C', 'IOLD', 1.0, 'USD', 'b', 1, 1,
                     TIMESTAMPTZ '2026-08-01T00:00:00Z')",
            [],
        )
        .unwrap();
        crate::store::ddl::refresh_enum(
            conn,
            "risk",
            "book",
            "risk_instrument_live",
            "risk_instrument_archive",
        )
        .unwrap();
        (store, ds)
    }

    fn count(conn: &Connection, _ds: &geode_core::schema::DatasetSpec, sql: &ScopeSql) -> i64 {
        conn.query_row(
            &format!(
                "select count(*) from risk_instrument_live where {}",
                sql.predicate
            ),
            duckdb::params_from_iter(sql.params.iter()),
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn a_text_filter_over_a_categorical_column_matches_the_dictionary_not_the_rows() {
        let (store, ds) = enum_fixture(); // books BK000..BK019 live; `book` categorical + textual
        let scope = Scope {
            text: Some("bk00".into()),
            ..Scope::default()
        };
        let sql = compile_scope(
            store.writer(),
            &scope,
            &ds,
            Grain::Instrument,
            &dims(),
            Era::live(),
        )
        .unwrap();
        assert!(
            sql.predicate.contains("string_split"),
            "the matches are bound as a literal list, not a subquery: {}",
            sql.predicate
        );
        assert!(
            !sql.predicate.contains("enum_range"),
            "the dictionary is resolved at compile time, not embedded in the predicate: {}",
            sql.predicate
        );
        // And it selects the same rows as the row scan would.
        let via_dict: i64 = count(store.writer(), &ds, &sql);
        let row_scan: i64 = store
            .writer()
            .query_row(
                "select count(*) from risk_instrument_live where \"book\" ilike '%bk00%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(via_dict, row_scan);
        assert!(via_dict > 0);
    }

    #[test]
    fn a_needle_matching_no_dictionary_value_compiles_to_false() {
        // `enum_fixture` marks only `book` textual, and it is categorical
        // — so a needle none of BK000..BK019, BK_01, BK%02, BK\03 can
        // meet drops the only term there is, and the whole filter must
        // collapse to `false` rather than silently falling through to
        // "no clause", which would select every row instead of none.
        let (store, ds) = enum_fixture();
        let scope = Scope {
            text: Some("zzz".into()),
            ..Scope::default()
        };
        let sql = compile_scope(
            store.writer(),
            &scope,
            &ds,
            Grain::Instrument,
            &dims(),
            Era::live(),
        )
        .unwrap();
        assert_eq!(
            sql.predicate, "false",
            "a needle matching no dictionary value must select nothing: {}",
            sql.predicate
        );
        assert!(sql.params.is_empty());
        assert_eq!(count(store.writer(), &ds, &sql), 0);
    }

    #[test]
    fn a_dictionary_match_binds_the_matching_values_not_the_pattern() {
        // `like_pattern` escapes `_`, so `bk_0` only meets `BK_01` —
        // none of BK000..BK019 (no literal underscore), BK%02 or BK\03.
        let (store, ds) = enum_fixture();
        let scope = Scope {
            text: Some("bk_0".into()),
            ..Scope::default()
        };
        let sql = compile_scope(
            store.writer(),
            &scope,
            &ds,
            Grain::Instrument,
            &dims(),
            Era::live(),
        )
        .unwrap();
        assert!(sql.predicate.contains("string_split"), "{}", sql.predicate);
        assert_eq!(
            sql.params,
            vec![Value::Text("BK_01".to_string())],
            "the bound value must be the matching dictionary entries, not the raw \
             pattern: {:?}",
            sql.params
        );
        let via_dict = count(store.writer(), &ds, &sql);
        let row_scan: i64 = store
            .writer()
            .query_row(
                "select count(*) from risk_instrument_live where \"book\" ilike ? escape '\\'",
                duckdb::params![like_pattern("bk_0")],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(via_dict, row_scan);
        assert_eq!(via_dict, 1);
    }

    /// `enum_fixture`'s dataset plus a plain textual key (`position_ref`,
    /// never categorical): the mixed case a desk actually has, where one
    /// row's key text is findable by a needle no dictionary value meets.
    fn enum_and_textual_key_fixture() -> (crate::store::Store, geode_core::schema::DatasetSpec) {
        let mut ds = carried_dataset();
        for c in ds.columns.iter_mut() {
            if c.name == "book" {
                c.categorical = true;
                c.textual = true;
            }
            if c.name == "position_ref" {
                c.textual = true;
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        std::mem::forget(dir);
        store.apply_schema(&ds).unwrap();
        let conn = store.writer();
        let insert = "insert into risk_instrument_live
                     (book, lhu, position_ref, counterparty, instrument_ref,
                      npv, currency, batch, source_file_id, gen_id, source_time)
                 values (?, 'L', ?, 'C', ?, 1.0, 'USD', 'b', 1, 1, now())";
        for i in 0..20 {
            conn.execute(
                insert,
                duckdb::params![format!("BK{i:03}"), format!("P{i}"), format!("I{i}")],
            )
            .unwrap();
        }
        conn.execute(insert, duckdb::params!["BK000", "PZZZFINDME", "IZZZ"])
            .unwrap();
        crate::store::ddl::refresh_enum(
            conn,
            "risk",
            "book",
            "risk_instrument_live",
            "risk_instrument_archive",
        )
        .unwrap();
        (store, ds)
    }

    #[test]
    fn a_needle_missing_from_the_dictionary_still_matches_a_plain_textual_key() {
        // `book`'s dictionary drops "zzzfindme" — no BK### value meets
        // it — but `position_ref`'s `ILIKE` term is not categorical, so
        // it survives the drop and still finds the one row.
        let (store, ds) = enum_and_textual_key_fixture();
        let scope = Scope {
            text: Some("zzzfindme".into()),
            ..Scope::default()
        };
        let sql = compile_scope(
            store.writer(),
            &scope,
            &ds,
            Grain::Instrument,
            &dims(),
            Era::live(),
        )
        .unwrap();
        assert_ne!(
            sql.predicate, "false",
            "the key's ILIKE term must survive the dictionary's drop: {}",
            sql.predicate
        );
        assert_eq!(count(store.writer(), &ds, &sql), 1);
    }

    #[test]
    fn the_rewrite_falls_back_to_the_row_scan_only_when_the_type_is_missing() {
        // Type existence is the only gate now (root-cause fix, spec §3.5
        // as amended) — not the era. Before the first load, or with the
        // type explicitly dropped, neither era can name it, and both
        // fall back; see
        // `a_text_filter_over_a_categorical_column_selects_an_archived_only_value_under_as_of`
        // for the case where the type *does* exist under as-of.
        let (store, ds) = enum_fixture();
        let scope = Scope {
            text: Some("bk00".into()),
            ..Scope::default()
        };
        store
            .writer()
            .execute_batch("drop type risk_book_enum")
            .unwrap();
        let sql = compile_scope(
            store.writer(),
            &scope,
            &ds,
            Grain::Instrument,
            &dims(),
            Era::live(),
        )
        .unwrap();
        assert!(
            !sql.predicate.contains("string_split"),
            "the dropped type must fall back to the row scan, not the dictionary: {}",
            sql.predicate
        );
        let archive = Era {
            kind: TableKind::Archive,
            generations: Some("true"),
        };
        let sql = compile_scope(
            store.writer(),
            &scope,
            &ds,
            Grain::Instrument,
            &dims(),
            archive,
        )
        .unwrap();
        assert!(
            !sql.predicate.contains("string_split"),
            "the dropped type must fall back to the row scan, not the dictionary: {}",
            sql.predicate
        );
    }

    #[test]
    fn a_text_filter_over_a_categorical_column_selects_an_archived_only_value_under_as_of() {
        // `BK_OLD` was never live — it exists only in the archive — the
        // case the era gate's own comment worried about. Root cause
        // fixed (spec §3.5 as amended): `refresh_enum` reads live and
        // archive, so the type carries it, and the rewrite is no longer
        // gated to the live era.
        let (store, ds) = archived_only_value_fixture();
        let scope = Scope {
            text: Some("old".into()),
            ..Scope::default()
        };
        let archive = Era {
            kind: TableKind::Archive,
            generations: Some("true"),
        };
        let sql = compile_scope(
            store.writer(),
            &scope,
            &ds,
            Grain::Instrument,
            &dims(),
            archive,
        )
        .unwrap();
        assert!(
            sql.predicate.contains("string_split"),
            "the archive era must take the dictionary path too: {}",
            sql.predicate
        );
        // The read path's opinions are law: the dictionary term must
        // select exactly what the row scan over the same relation would.
        let relation = archive.relation(&ds.name, Grain::Instrument);
        let via_dict: i64 = store
            .writer()
            .query_row(
                &format!("select count(*) from {relation} where {}", sql.predicate),
                duckdb::params_from_iter(sql.params.iter()),
                |r| r.get(0),
            )
            .unwrap();
        let row_scan: i64 = store
            .writer()
            .query_row(
                &format!("select count(*) from {relation} where \"book\" ilike '%old%'"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            via_dict, row_scan,
            "the dictionary term must select exactly what the row scan does"
        );
        assert!(via_dict > 0, "the archived-only value must be found");
    }

    proptest! {
        #[test]
        fn dictionary_and_row_scan_agree_for_any_needle(needle in "[a-zA-Z0-9%_\\\\]{0,6}") {
            let (store, ds) = enum_fixture();
            let scope = Scope { text: Some(needle.clone()), ..Scope::default() };
            let sql = compile_scope(store.writer(), &scope, &ds, Grain::Instrument, &dims(), Era::live()).unwrap();
            let via_dict = count(store.writer(), &ds, &sql);
            let pattern = like_pattern(&needle);
            let row_scan: i64 = store.writer().query_row(
                "select count(*) from risk_instrument_live where \"book\" ilike ? escape '\\'",
                duckdb::params![pattern], |r| r.get(0)).unwrap();
            prop_assert_eq!(via_dict, row_scan);
        }
    }

    // ---- DictionaryCache (dictionary resolves once per statement) ----

    #[test]
    fn a_cache_resolves_each_dictionary_once_per_statement() {
        // `enum_fixture` declares exactly one categorical textual column
        // (`book`), so one statement's text block should cost exactly
        // two catalog round-trips: `existing_enum_types` once, and
        // `dictionary_matches` once for `book`.
        let (store, ds) = enum_fixture();
        let scope = Scope {
            text: Some("bk00".into()),
            ..Scope::default()
        };
        let mut cache = DictionaryCache::default();
        compile_scope_cached(
            store.writer(),
            &scope,
            &ds,
            Grain::Instrument,
            &dims(),
            Era::live(),
            &mut cache,
        )
        .unwrap();
        assert_eq!(
            cache.lookups, 2,
            "one enum_types lookup plus one dictionary match for the fixture's one \
             categorical textual column"
        );
        let after_first = cache.lookups;

        // A second grain of the *same statement*, same cache: must not
        // pay for either lookup again.
        compile_scope_cached(
            store.writer(),
            &scope,
            &ds,
            Grain::Position,
            &dims(),
            Era::live(),
            &mut cache,
        )
        .unwrap();
        assert_eq!(
            cache.lookups, after_first,
            "a second grain of the same statement must reuse the cache, not re-resolve"
        );
    }

    #[test]
    fn a_cache_keys_matches_by_pattern_not_type_alone() {
        // `enum_fixture` seeds `BK_01` (a literal underscore) alongside
        // `BK000..BK019`. `like_pattern` escapes the underscore in the
        // needle, so "bk_0" matches only `BK_01` — one dictionary value —
        // while "zzz" matches nothing. A cache keyed on the ENUM type
        // alone would answer the second compile with the first needle's
        // cached match instead of resolving `zzz` fresh.
        let (store, ds) = enum_fixture();
        let mut cache = DictionaryCache::default();

        let first = Scope {
            text: Some("bk_0".into()),
            ..Scope::default()
        };
        let sql1 = compile_scope_cached(
            store.writer(),
            &first,
            &ds,
            Grain::Instrument,
            &dims(),
            Era::live(),
            &mut cache,
        )
        .unwrap();
        assert!(
            count(store.writer(), &ds, &sql1) > 0,
            "the first needle must find BK_01"
        );

        let second = Scope {
            text: Some("zzz".into()),
            ..Scope::default()
        };
        let sql2 = compile_scope_cached(
            store.writer(),
            &second,
            &ds,
            Grain::Instrument,
            &dims(),
            Era::live(),
            &mut cache,
        )
        .unwrap();
        assert_eq!(
            sql2.predicate, "false",
            "a cache keyed on the type alone would reuse 'bk_0's matches for 'zzz': {}",
            sql2.predicate
        );
        assert!(
            !sql2
                .params
                .iter()
                .any(|p| matches!(p, Value::Text(t) if t.contains("BK_01"))),
            "the second compile's params must not carry the first needle's bound matches: {:?}",
            sql2.params
        );
    }

    #[test]
    fn a_cache_keys_matches_by_type_not_pattern_alone() {
        // Review round 1, Major 1 — the mirror of the test above: a key
        // that collapsed to the pattern alone, dropping which column's
        // ENUM type it names, would let `counterparty`'s lookup hit
        // `book`'s already-cached entry for the same needle. Both
        // columns' dictionaries contain exactly one match for "match" —
        // `book`'s is `BKMATCH`, `counterparty`'s is `CPMATCH` — so a
        // collision is visible as the two bound value lists becoming
        // equal instead of staying distinct.
        let (store, ds) = two_categorical_columns_fixture();
        let scope = Scope {
            text: Some("match".into()),
            ..Scope::default()
        };
        let mut cache = DictionaryCache::default();
        let sql = compile_scope_cached(
            store.writer(),
            &scope,
            &ds,
            Grain::Instrument,
            &dims(),
            Era::live(),
            &mut cache,
        )
        .unwrap();
        // One bound literal-list value per categorical textual column,
        // in the schema's declared column order: `book`, then
        // `counterparty` (`carried_dataset`'s TOML lists `book` first).
        let bound: Vec<&str> = sql
            .params
            .iter()
            .filter_map(|p| match p {
                Value::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            bound.len(),
            2,
            "one bound value per categorical textual column: {bound:?}"
        );
        assert!(
            bound[0].contains("BKMATCH"),
            "book's term must bind book's own match: {bound:?}"
        );
        assert!(
            bound[1].contains("CPMATCH"),
            "counterparty's term must bind counterparty's own match, not book's: {bound:?}"
        );
        assert_ne!(
            bound[0], bound[1],
            "book's and counterparty's dictionary matches must not collide: {bound:?}"
        );
    }

    proptest! {
        #[test]
        fn compile_scope_and_compile_scope_cached_agree(needle in "[a-zA-Z0-9%_\\\\]{0,6}") {
            // The read path's opinions are law: a caller-supplied cache
            // must be invisible to the property test's contract. Both
            // entry points, on a fresh (cold) cache, must compile the
            // identical statement.
            let (store, ds) = enum_fixture();
            let scope = Scope { text: Some(needle.clone()), ..Scope::default() };
            let via_wrapper = compile_scope(
                store.writer(), &scope, &ds, Grain::Instrument, &dims(), Era::live(),
            ).unwrap();
            let mut cache = DictionaryCache::default();
            let via_cached = compile_scope_cached(
                store.writer(), &scope, &ds, Grain::Instrument, &dims(), Era::live(), &mut cache,
            ).unwrap();
            prop_assert_eq!(via_wrapper.predicate.clone(), via_cached.predicate.clone());
            prop_assert_eq!(via_wrapper.params.clone(), via_cached.params.clone());
            prop_assert_eq!(via_wrapper.semantics.clone(), via_cached.semantics.clone());

            // Review round 1, Major 2: the case above never exercises a
            // *warm* cache -- both arms start cold, so it can only fail
            // if the wrapper forwards a different argument list, not if
            // a hit ever returned something other than what a miss
            // would have. Compile the same statement again through the
            // now-warm `cache` and require the identical result: this is
            // the one place a `matches`/`enum_types` hit is actually
            // exercised and checked against a real answer, rather than
            // discarded (as `a_cache_resolves_each_dictionary_once_per_
            // statement` does, asserting only on `cache.lookups`).
            let via_warm_cache = compile_scope_cached(
                store.writer(), &scope, &ds, Grain::Instrument, &dims(), Era::live(), &mut cache,
            ).unwrap();
            prop_assert_eq!(via_cached.predicate, via_warm_cache.predicate);
            prop_assert_eq!(via_cached.params, via_warm_cache.params);
            prop_assert_eq!(via_cached.semantics, via_warm_cache.semantics);
        }
    }
}
