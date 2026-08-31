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
    /// Live reads the live table and nothing else. As-of reads the archive
    /// **and** live: the generation a partition holds *now* is in live and
    /// nowhere else, so a query as of any moment after that generation was
    /// published — including "as of an hour ago" for a book that refreshed
    /// this morning — has to find it there. Reading the archive alone
    /// answers such a query with the partition's *previous* generation, or
    /// with nothing at all for a partition published only once, and says
    /// nothing either way. The generation predicate is what keeps the two
    /// sides from both contributing; live carries `gen_id` and
    /// `source_time` precisely so it can be filtered the same way (§4.2).
    pub fn relation(&self, dataset: &str, grain: Grain) -> String {
        match self.kind {
            TableKind::Live => table_name(dataset, grain, TableKind::Live),
            TableKind::Archive => format!(
                "(select * from {} union all select * from {})",
                table_name(dataset, grain, TableKind::Archive),
                table_name(dataset, grain, TableKind::Live)
            ),
        }
    }
}

/// Whether `column` can be evaluated on `grain`'s own rows: a dimension
/// key it carries, or a measure or attribute declared at it.
fn evaluable_at(ds: &DatasetSpec, dims: &DerivedDimensions, grain: Grain, column: &str) -> bool {
    let base = dims.base_column(column);
    grain.dimension_key_columns().contains(&base)
        || ds.column(base).and_then(|c| c.grain()) == Some(grain)
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
        let carried = Grain::ALL
            .iter()
            .any(|g| g.dimension_key_columns().contains(&base));
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
/// The probe reads the same era as its caller. Reading live from inside
/// an as-of query mixes today's data into a historical answer — and does
/// it silently, because the numbers still look like numbers (spec §6.5).
fn membership(ds: &DatasetSpec, grain: Grain, probe: Grain, era: Era<'_>, inner: &str) -> String {
    let join = shared_keys(grain, probe)
        .iter()
        .map(|k| format!("probe.\"{k}\" is not distinct from base.\"{k}\""))
        .collect::<Vec<_>>()
        .join(" and ");
    let mut terms = vec![join, inner.to_string()];
    if let Some(generations) = era.generations {
        terms.push(format!("({generations})"));
    }
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
pub fn compile_scope(
    // Kept so the signature does not change when a predicate kind needs
    // the connection again; nothing does today.
    _conn: &Connection,
    scope: &Scope,
    ds: &DatasetSpec,
    grain: Grain,
    dims: &DerivedDimensions,
    era: Era<'_>,
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
        let base = dims.base_column(&sel.column).to_string();

        // A selection on a derived dimension names *derived* values, but
        // the stored column holds source values — so the selection has to
        // be translated back through the map. Binding the derived values
        // against the source column compiles cleanly and silently matches
        // nothing, which is the worst way for this to fail (spec §6.8).
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
            // Selected a derived value the map does not produce: nothing
            // can match, and saying so beats an empty `in ()`.
            return Ok(nothing());
        }
        let clause = (
            format!("\"{base}\" in (select unnest(string_split(?, '{SELECTION_DELIMITER}')))"),
            vec![Value::Text(values.join(SELECTION_DELIMITER))],
        );
        place(&mut r, ds, dims, grain, &[sel.column.as_str()], clause)?;
    }

    // 2. Text filter: OR of ILIKE over declared textual columns.
    //
    // Each column is routed on its own, because the OR cannot be split:
    // a textual column this grain does not carry becomes its own
    // membership term inside the OR, and the whole filter is one direct
    // clause. Leaving such columns out — the earlier choice — silently
    // applied the filter to the fine-grained measures and not to the
    // coarse ones on the same row, and marked nothing (spec §6.3).
    if let Some(text) = &scope.text {
        let pattern = Value::Text(like_pattern(text));
        let mut terms: Vec<String> = Vec::new();
        let mut term_params: Vec<Value> = Vec::new();
        for col in ds.textual_columns() {
            let name = col.name.as_str();
            let test = format!("\"{name}\" ilike ? escape '\\'");
            match route(ds, dims, grain, &[name])? {
                None => terms.push(test),
                Some(probe) => {
                    if is_membership(grain, probe) && !r.semi_dimensions.iter().any(|s| s == name) {
                        r.semi_dimensions.push(name.to_string());
                    }
                    terms.push(membership(ds, grain, probe, era, &test));
                }
            }
            term_params.push(pattern.clone());
        }
        if !terms.is_empty() {
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
            role: geode_core::schema::ColumnRole::Attribute {
                grain: Grain::Instrument,
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
}
