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
//! A predicate naming a column that does not exist at the requested grain
//! becomes a semi-join against the grain where it does, and the result is
//! marked `SemiJoined`: "positions that have SPX risk" is not "the SPX
//! share of the position" (spec §6.3).

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
}

/// `probe` is the grain a finer-than-`grain` predicate is tested against.
/// It must be a grain the dataset declares — only those have tables — so
/// the caller passes the spine grain rather than assuming the finest.
pub fn compile_scope(
    // Kept so the signature does not change when a predicate kind needs
    // the connection again; nothing does today.
    _conn: &Connection,
    scope: &Scope,
    ds: &DatasetSpec,
    grain: Grain,
    dims: &DerivedDimensions,
    probe: Grain,
    era: Era<'_>,
) -> Result<ScopeSql, StoreError> {
    // A contradiction selects nothing, and must say so in SQL. Returning
    // early matters: the contradicted dimension has already been dropped
    // from `dimensions`, so compiling the rest would produce a predicate
    // that is *wider* than either layer asked for (see `Scope::and_then`).
    if scope.impossible {
        return Ok(ScopeSql {
            predicate: "false".to_string(),
            params: Vec::new(),
            semantics: ScopeSemantics::Direct,
        });
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
    let mut direct: Vec<Clause> = Vec::new();
    let mut finer: Vec<Clause> = Vec::new();
    let mut semi_dimensions: Vec<String> = Vec::new();

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
            return Ok(ScopeSql {
                predicate: "false".to_string(),
                params: Vec::new(),
                semantics: ScopeSemantics::Direct,
            });
        }
        let clause = (
            format!("\"{base}\" in (select unnest(string_split(?, '{SELECTION_DELIMITER}')))"),
            vec![Value::Text(values.join(SELECTION_DELIMITER))],
        );
        if grain.key_columns().contains(&base.as_str()) {
            direct.push(clause);
        } else {
            finer.push(clause);
            semi_dimensions.push(base);
        }
    }

    // 2. Text filter: OR of ILIKE over declared textual columns.
    if let Some(text) = &scope.text {
        let pattern = format!("%{text}%");
        let mut terms = Vec::new();
        let mut term_params = Vec::new();
        for col in ds.textual_columns() {
            // Only columns present at this grain; a textual column from a
            // finer grain would need its own semi-join, and the global
            // text filter is not worth that complexity (spec §4.1).
            if grain.key_columns().contains(&col.name.as_str()) {
                terms.push(format!("\"{}\" ilike ?", col.name));
                term_params.push(Value::Text(pattern.clone()));
            }
        }
        if !terms.is_empty() {
            direct.push((format!("({})", terms.join(" or ")), term_params));
        }
    }

    // 3. Expression filter: AST lowered, literals bound.
    if let Some(expr) = &scope.expression {
        let mut expr_params = Vec::new();
        let rendered = render_expr(expr, &mut expr_params, dims)?;
        // "Finer" means the column lives at a finer grain — not merely
        // that it is absent from this grain's *key*. A measure or
        // attribute declared at this grain is on this very table, so it
        // is a direct predicate; routing it through a semi-join badges
        // the result "positions that have…" when it is nothing of the
        // kind, and leaves the column as an unqualified outer reference
        // that would silently rebind if the probe grain ever carried a
        // column of the same name.
        let is_finer = |c: &str| {
            let base = dims.base_column(c);
            if grain.key_columns().contains(&base) {
                return false;
            }
            // Declared at this grain — a measure or attribute of it.
            match ds.column(base).and_then(|col| col.grain()) {
                Some(g) => g != grain,
                // Not declared at any grain (a plain dimension), so it is
                // finer exactly when it is not a key column here.
                None => true,
            }
        };
        let mentions_finer = expr.columns().iter().any(|c| is_finer(c));
        if mentions_finer {
            for c in expr.columns() {
                let base = dims.base_column(c);
                if is_finer(c) && !semi_dimensions.iter().any(|s| s == base) {
                    semi_dimensions.push(base.to_string());
                }
            }
            finer.push((rendered, expr_params));
        } else {
            direct.push((rendered, expr_params));
        }
    }

    // Finer predicates apply as a membership test against the grain where
    // those columns exist. The finest grain always carries every key
    // column, so it is the safe target.
    if !finer.is_empty() {
        // `is not distinct from`, not `=`: the key columns are matching
        // two rows of the *same* table, so a NULL here is a real value on
        // both sides rather than a rolled-up placeholder. Plain equality
        // would make a position with no LHU fail its own semi-join, and
        // the row would still be present carrying a coarse measure of
        // NULL — visibly inconsistent rather than merely absent.
        let join = grain
            .key_columns()
            .iter()
            .map(|k| format!("probe.\"{k}\" is not distinct from base.\"{k}\""))
            .collect::<Vec<_>>()
            .join(" and ");
        // The probe must read the same era as its caller. Reading live
        // from inside an as-of query mixes today's data into a historical
        // answer — and does it silently, because the numbers still look
        // like numbers (spec §6.5).
        let mut where_terms = vec![
            join,
            finer
                .iter()
                .map(|(clause, _)| clause.clone())
                .collect::<Vec<_>>()
                .join(" and "),
        ];
        if let Some(generations) = era.generations {
            where_terms.push(format!("({generations})"));
        }
        // The wrapper is appended last, so its values follow every direct
        // clause's — which is exactly what carrying them together gives.
        let finer_params: Vec<Value> = finer.iter().flat_map(|(_, p)| p.clone()).collect();
        direct.push((
            format!(
                "exists (select 1 from {} probe where {})",
                table_name(&ds.name, probe, era.kind),
                where_terms.join(" and ")
            ),
            finer_params,
        ));
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
            Grain::UnderlyingPair,
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
            Grain::UnderlyingPair,
            Era::live(),
        )
        .unwrap();
        let b = compile_scope(
            store.writer(),
            &large,
            &dataset(),
            Grain::Underlying,
            &dims(),
            Grain::UnderlyingPair,
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
            Grain::UnderlyingPair,
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
