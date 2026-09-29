//! The parity contract between the in-process scope evaluator
//! (`geode_core::scope::Scope::matches`) and the SQL lowering in
//! [`super::scope_sql`]. The SQL is the authority: every case runs through
//! `compile_scope` against a DuckDB table and through `matches` over the
//! same rows, and the two row sets must be equal. Where DuckDB refuses a
//! statement, `matches` must refuse too.
//!
//! The fixture's cases are the record of DuckDB's behaviour the evaluator
//! copies: cross-type comparisons, LIKE with no ESCAPE clause, byte-order
//! text comparison, NULL under `not`, and the derived-dimension constants.

use super::scope_sql::{Era, compile_scope};
use duckdb::Connection;
use geode_core::config::{LayerDoc, merge_docs};
use geode_core::dimensions::DerivedDimensions;
use geode_core::document::Value;
use geode_core::schema::{Aggregate, ColumnRole, ColumnSpec, ColumnType, DatasetSpec, Grain};
use geode_core::scope::{DimensionSelection, RowValues, Scope, parse_expr};
use std::collections::{BTreeSet, HashMap};

/// One fixture row: its id and each column's value, `None` for NULL.
struct Row {
    id: i64,
    values: HashMap<&'static str, Value>,
}

impl RowValues for Row {
    fn value(&self, column: &str) -> Option<Value> {
        self.values.get(column).cloned()
    }
}

fn column(name: &str, ty: ColumnType, textual: bool, role: ColumnRole) -> ColumnSpec {
    ColumnSpec {
        name: name.to_string(),
        source_name: None,
        ty,
        required: false,
        textual,
        categorical: false,
        role,
    }
}

/// Every column is evaluable on the underlying grain's own rows, so the
/// compiled predicate is direct and can run against the one table `t`.
fn dataset(textual: bool) -> DatasetSpec {
    let dim = ColumnRole::Dimension {
        grain: Some(Grain::Underlying),
    };
    DatasetSpec {
        name: "t".into(),
        columns: vec![
            column("und", ColumnType::Utf8, textual, dim),
            column("cur", ColumnType::Utf8, textual, dim),
            column("kind", ColumnType::Utf8, false, dim),
            column("code", ColumnType::Utf8, false, dim),
            column("strike", ColumnType::F64, false, dim),
            column(
                "qty",
                ColumnType::I64,
                false,
                ColumnRole::Attribute {
                    grain: Some(Grain::Underlying),
                },
            ),
            column(
                "expiry",
                ColumnType::Date,
                false,
                ColumnRole::Attribute {
                    grain: Some(Grain::Underlying),
                },
            ),
            column(
                "npv",
                ColumnType::F64,
                false,
                ColumnRole::Measure {
                    grain: Grain::Underlying,
                    aggregate: Aggregate::Sum,
                },
            ),
        ],
        ..DatasetSpec::default()
    }
}

/// `region` over `und`: SPX and NDX are US, SX5E is EU. Nothing maps to
/// APAC, and lower-case `spx` maps to nothing.
fn dims() -> DerivedDimensions {
    let text = r#"
[region]
from = "und"
values = { US = ["SPX", "NDX"], EU = ["SX5E"] }
"#;
    let doc = merge_docs(
        "dimensions",
        &[LayerDoc::builtin("dimensions", text).unwrap()],
    );
    let (dims, diags) = DerivedDimensions::from_doc(&doc);
    assert!(diags.is_empty(), "{diags:?}");
    dims
}

/// One fixture row as literals; `None` is NULL.
struct Fixture {
    und: Option<&'static str>,
    cur: Option<&'static str>,
    kind: Option<&'static str>,
    /// Numeric text in a text column: every value casts to DOUBLE, so a
    /// number compared with it succeeds where `und` (with `SPX`) fails.
    code: Option<&'static str>,
    strike: Option<f64>,
    qty: Option<i64>,
    expiry: Option<&'static str>,
    npv: Option<f64>,
}

#[allow(clippy::too_many_arguments)]
const fn row(
    und: Option<&'static str>,
    cur: Option<&'static str>,
    kind: Option<&'static str>,
    code: Option<&'static str>,
    strike: Option<f64>,
    qty: Option<i64>,
    expiry: Option<&'static str>,
    npv: Option<f64>,
) -> Fixture {
    Fixture {
        und,
        cur,
        kind,
        code,
        strike,
        qty,
        expiry,
        npv,
    }
}

/// Ids are positions from 1. Mixed case, NULLs in every column, LIKE
/// wildcards and a backslash in the data, a numeric spelling in a text
/// column, a non-ASCII value, and a NaN measure.
#[rustfmt::skip]
const ROWS: [Fixture; 12] = [
    row(Some("SPX"), Some("USD"), Some("call"), Some("100"), Some(100.0), Some(1), Some("2026-12-18"), Some(5.0)),
    row(Some("NDX"), Some("usd"), Some("put"), Some("1e2"), Some(150.5), Some(2), Some("2027-01-15"), None),
    row(Some("SX5E"), Some("EUR"), Some("Call"), Some(" 7 "), None, Some(3), None, Some(-2.0)),
    row(Some("spx"), None, Some("put"), Some("-3"), Some(90.0), Some(-1), Some("2026-12-18"), Some(0.0)),
    row(Some("50%off"), Some("GBP"), Some("cap_floor"), Some("2.50"), Some(100.0), Some(10), Some("2027-03-19"), Some(1.5)),
    row(Some("A_B"), Some("USD"), Some("capXfloor"), None, Some(200.0), Some(100), None, Some(f64::NAN)),
    row(None, Some("EUR"), Some("call"), Some("100.0"), Some(1.0), Some(0), Some("2026-06-30"), Some(3.0)),
    row(Some("RUT"), Some("JPY"), Some("a\\b"), Some("7"), Some(1000.0), Some(2), Some("2028-12-15"), Some(7.0)),
    row(Some("SX5E"), Some("EUR"), Some("PUT"), None, Some(99.99), None, Some("2027-06-18"), Some(4.0)),
    row(Some("100"), Some("Usd"), None, Some("0"), Some(2.5), Some(3), Some("2026-12-18"), Some(-0.5)),
    row(Some("Ärger"), Some("CHF"), Some("call"), Some("+5"), Some(100.5), Some(1), Some("2027-01-15"), Some(2.0)),
    row(None, None, Some("put"), Some("-0"), Some(-5.0), Some(-3), Some("2026-12-18"), Some(1.0)),
];

fn rows() -> Vec<Row> {
    ROWS.iter()
        .enumerate()
        .map(|(i, f)| {
            let mut values = HashMap::new();
            for (k, v) in [
                ("und", f.und),
                ("cur", f.cur),
                ("kind", f.kind),
                ("code", f.code),
            ] {
                if let Some(v) = v {
                    values.insert(k, Value::Utf8(v.to_string()));
                }
            }
            if let Some(v) = f.strike {
                values.insert("strike", Value::F64(v));
            }
            if let Some(v) = f.qty {
                values.insert("qty", Value::I64(v));
            }
            if let Some(v) = f.expiry {
                values.insert(
                    "expiry",
                    Value::Date(chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d").unwrap()),
                );
            }
            if let Some(v) = f.npv {
                values.insert("npv", Value::F64(v));
            }
            Row {
                id: i as i64 + 1,
                values,
            }
        })
        .collect()
}

fn connection() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "create table t(id bigint, und varchar, cur varchar, kind varchar, code varchar,
                        strike double, qty bigint, expiry date, npv double);",
    )
    .unwrap();
    for (i, f) in ROWS.iter().enumerate() {
        conn.execute(
            "insert into t values (?, ?, ?, ?, ?, ?, ?, cast(? as date), ?)",
            duckdb::params![
                i as i64 + 1,
                f.und,
                f.cur,
                f.kind,
                f.code,
                f.strike,
                f.qty,
                f.expiry,
                f.npv
            ],
        )
        .unwrap();
    }
    conn
}

/// The ids the SQL path keeps, or its error (compile or execution).
fn sql_ids(
    conn: &Connection,
    scope: &Scope,
    ds: &DatasetSpec,
    dims: &DerivedDimensions,
) -> Result<BTreeSet<i64>, String> {
    let sql = compile_scope(conn, scope, ds, Grain::Underlying, dims, Era::live())
        .map_err(|e| e.to_string())?;
    let text = format!("select id from t where {} order by id", sql.predicate);
    let mut stmt = conn.prepare(&text).map_err(|e| e.to_string())?;
    let ids = stmt
        .query_map(duckdb::params_from_iter(sql.params.iter()), |r| {
            r.get::<_, i64>(0)
        })
        .map_err(|e| e.to_string())?;
    ids.collect::<Result<BTreeSet<_>, _>>()
        .map_err(|e| e.to_string())
}

/// The ids the evaluator keeps, or the first row's error.
fn eval_ids(
    rows: &[Row],
    scope: &Scope,
    ds: &DatasetSpec,
    dims: &DerivedDimensions,
) -> Result<BTreeSet<i64>, String> {
    let mut out = BTreeSet::new();
    for row in rows {
        if scope.matches(row, ds, dims)? {
            out.insert(row.id);
        }
    }
    Ok(out)
}

fn expr(text: &str) -> Scope {
    Scope {
        expression: Some(parse_expr(text).unwrap_or_else(|e| panic!("{text}: {e}"))),
        ..Scope::default()
    }
}

fn text(needle: &str) -> Scope {
    Scope {
        text: Some(needle.into()),
        ..Scope::default()
    }
}

fn select(column: &str, values: &[&str]) -> Scope {
    Scope {
        dimensions: vec![DimensionSelection {
            column: column.into(),
            values: values.iter().map(|v| v.to_string()).collect(),
        }],
        ..Scope::default()
    }
}

/// Run every case through both paths; collect every disagreement so one
/// run shows them all. `GEODE_PARITY_TRACE=1` prints the SQL outcome of
/// each case, which is how the DuckDB facts in the report were read.
fn assert_parity(cases: &[(&str, Scope)], ds: &DatasetSpec) {
    let conn = connection();
    let dims = dims();
    let rows = rows();
    let trace = std::env::var_os("GEODE_PARITY_TRACE").is_some();
    let mut failures = Vec::new();
    for (name, scope) in cases {
        let sql = sql_ids(&conn, scope, ds, &dims);
        let eval = eval_ids(&rows, scope, ds, &dims);
        if trace {
            println!("{name:<40} sql={sql:?}");
        }
        let agree = match (&sql, &eval) {
            (Ok(a), Ok(b)) => a == b,
            (Err(_), Err(_)) => true,
            _ => false,
        };
        if !agree {
            failures.push(format!("{name}: sql={sql:?} eval={eval:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases disagree:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

#[test]
fn evaluate_agrees_with_scope_sql_on_every_operator() {
    let cases: Vec<(&str, Scope)> = [
        // Text comparisons: byte order, case-sensitive.
        ("text =", "und = 'SPX'"),
        ("text = other case", "und = 'spx'"),
        ("text !=", "und != 'SPX'"),
        ("text <>", "und <> 'SPX'"),
        ("text <", "cur < 'USD'"),
        ("text <=", "cur <= 'USD'"),
        ("text >", "cur > 'EUR'"),
        ("text >= lower", "cur >= 'usd'"),
        ("text < non-ascii", "und < 'Z'"),
        // Numeric comparisons, f64 and i64.
        ("f64 =", "strike = 100"),
        ("f64 !=", "strike != 100"),
        ("f64 <", "strike < 100"),
        ("f64 <=", "strike <= 100"),
        ("f64 >", "strike > 100"),
        ("f64 >= fraction", "strike >= 100.5"),
        ("f64 negative", "npv = -2"),
        ("f64 > 0", "npv > 0"),
        ("i64 =", "qty = 2"),
        ("i64 > fraction", "qty > 1.5"),
        ("i64 = fraction", "qty = 2.5"),
        ("i64 < negative fraction", "qty < -0.5"),
        ("i64 >= fraction", "qty >= 2.5"),
        // Cross-type: a number against text, text against a number.
        ("number vs text =", "und = 100"),
        ("number vs text = fraction", "und = 100.5"),
        ("number vs text <", "cur < 5"),
        ("text vs f64 = int", "strike = '100'"),
        ("text vs f64 = decimal", "strike = '100.0'"),
        ("text vs f64 >", "strike > '99.5'"),
        ("text vs f64 junk", "strike = 'abc'"),
        ("text vs f64 padded", "strike = ' 100 '"),
        ("text vs i64 =", "qty = '2'"),
        ("text vs i64 fraction", "qty = '2.5'"),
        ("text vs i64 junk", "qty = 'x'"),
        ("text vs i64 rounds down", "qty = '1.4'"),
        ("text vs i64 rounds negative half", "qty = '-2.5'"),
        ("text vs i64 exponent", "qty = '1e1'"),
        ("in text vs i64 rounds", "qty in ('2.5')"),
        ("in text and number vs i64", "qty in ('2.5', 1)"),
        ("number vs text !=", "und != 100"),
        ("in bool vs text", "und in (true)"),
        // A text column whose every value is numeric text: `=` casts it.
        ("number vs numeric text =", "code = 100"),
        ("number vs numeric text !=", "code != 7"),
        ("in number vs numeric text", "code in (7, 100)"),
        ("in text and number vs numeric text", "code in ('7', 100)"),
        ("text vs numeric text", "code = '100'"),
        ("number vs numeric text <", "code < 5"),
        ("like numeric text", "code like '1%'"),
        // NaN: DuckDB orders it above every number and equal to itself.
        ("NaN >", "npv > 5"),
        ("NaN !=", "npv != 5"),
        ("NaN = text", "npv = 'NaN'"),
        ("NaN <", "npv < 1000"),
        // Booleans.
        ("bool vs f64", "strike = true"),
        ("bool vs i64", "qty = true"),
        ("bool vs text", "und = true"),
        // Dates.
        ("date = text", "expiry = '2026-12-18'"),
        ("date < text", "expiry < '2027-01-01'"),
        ("date junk", "expiry = 'nope'"),
        ("date vs number", "expiry = 20261218"),
        ("date like", "expiry like '2026%'"),
        // LIKE: case-insensitive, no ESCAPE clause.
        ("like %", "und like 'S%'"),
        ("like lower", "und like 's%'"),
        ("like _", "und like '_DX'"),
        ("like exact", "cur like 'usd'"),
        ("like literal % in data", "und like '50%'"),
        ("like backslash-percent", "und like '%\\%%'"),
        ("like backslash literal", "kind like 'a\\b'"),
        ("like _ over non-ascii", "und like '_rger'"),
        ("like non-ascii case", "und like 'ärger'"),
        ("like underscore in data", "kind like 'cap_floor'"),
        ("like on f64", "strike like '100%'"),
        ("like on i64", "qty like '1%'"),
        ("like number literal", "und like 100"),
        // Membership.
        ("in text", "und in ('SPX', 'NDX')"),
        ("in text with NULL row", "cur in ('USD', 'EUR')"),
        ("in f64", "strike in (100, 90)"),
        ("in i64 fraction", "qty in (1, 2.5)"),
        ("in number vs text", "und in (100)"),
        ("in text vs f64", "strike in ('100', '90')"),
        ("in text junk vs f64", "strike in ('100', 'abc')"),
        ("in mixed literals text col", "und in ('SPX', 100)"),
        ("in mixed literals f64 col", "strike in (100, '90')"),
        ("not in", "not (cur in ('USD'))"),
        // Kleene logic.
        ("not", "not (cur = 'USD')"),
        ("or", "cur = 'USD' or strike > 100"),
        ("and", "cur = 'USD' and strike > 100"),
        ("not or", "not (cur = 'USD' or strike > 100)"),
        ("not and", "not (cur = 'USD' and strike > 100)"),
        ("or null both", "cur = 'EUR' or npv > 0"),
        ("double not", "not (not (cur = 'USD'))"),
        // Derived dimension in expressions.
        ("derived =", "region = 'US'"),
        ("derived !=", "region != 'US'"),
        ("derived <>", "region <> 'EU'"),
        ("derived in", "region in ('EU', 'APAC')"),
        ("derived = unmapped", "region = 'APAC'"),
        ("derived != unmapped", "region != 'APAC'"),
        ("derived in unmapped", "region in ('APAC')"),
        ("not derived =", "not (region = 'US')"),
        ("not derived != unmapped", "not (region != 'APAC')"),
        ("derived = number", "region = 1"),
        ("derived in number", "region in (1, 'US')"),
        ("derived >", "region > 'US'"),
        ("derived like", "region like 'U%'"),
        // Errors and their interaction with constants.
        ("unknown column", "nope = 1"),
        (
            "false constant and junk cast",
            "region = 'APAC' and strike = 'abc'",
        ),
        (
            "true constant or junk cast",
            "region != 'APAC' or strike = 'abc'",
        ),
        (
            "false constant and binder error",
            "region = 'APAC' and cur < 5",
        ),
        (
            "true constant or binder error",
            "region != 'APAC' or strike like '1%'",
        ),
        (
            "false constant and date cast",
            "region = 'APAC' and expiry = 20261218",
        ),
        (
            "false constant and row cast",
            "region = 'APAC' and und = 100",
        ),
        (
            "not true constant and junk cast",
            "not (region != 'APAC') and strike = 'abc'",
        ),
        (
            "constant nested in or",
            "(region = 'APAC' and strike = 'abc') or und = 'SPX'",
        ),
        (
            "false constant or junk cast",
            "region = 'APAC' or strike = 'abc'",
        ),
    ]
    .into_iter()
    .map(|(name, e)| (name, expr(e)))
    .chain([
        // Text filter.
        ("text filter substring", text("PX")),
        ("text filter mixed case", text("spx")),
        ("text filter in cur", text("eUr")),
        ("text filter %", text("%")),
        ("text filter _", text("_")),
        ("text filter backslash", text("\\")),
        ("text filter non-textual only", text("cap")),
        ("text filter numeric only", text("150")),
        ("text filter numeric spelling", text("100")),
        ("text filter non-ascii", text("ä")),
        ("text filter empty", text("")),
        // Dimension selections.
        ("select plain", select("und", &["SPX", "NDX"])),
        ("select with NULLs", select("cur", &["USD"])),
        ("select case", select("cur", &["usd"])),
        ("select derived", select("region", &["US"])),
        ("select derived two", select("region", &["US", "EU"])),
        ("select derived unmapped", select("region", &["APAC"])),
        ("select derived mixed", select("region", &["APAC", "EU"])),
        ("select empty", select("und", &[])),
        ("select f64", select("strike", &["100"])),
        ("select f64 decimal", select("strike", &["100.0", "2.5"])),
        ("select i64", select("qty", &["2"])),
        ("select date", select("expiry", &["2026-12-18"])),
        ("select unknown", select("nope", &["x"])),
        // Composition.
        (
            "select and expr and text",
            Scope {
                dimensions: vec![DimensionSelection {
                    column: "region".into(),
                    values: vec!["US".into(), "EU".into()],
                }],
                text: Some("x".into()),
                expression: Some(parse_expr("strike > 95").unwrap()),
                ..Scope::default()
            },
        ),
        (
            "unmapped select beats junk cast",
            Scope {
                dimensions: vec![DimensionSelection {
                    column: "region".into(),
                    values: vec!["APAC".into()],
                }],
                expression: Some(parse_expr("strike = 'abc'").unwrap()),
                ..Scope::default()
            },
        ),
        (
            "impossible",
            Scope {
                impossible: true,
                expression: Some(parse_expr("strike = 'abc'").unwrap()),
                ..Scope::default()
            },
        ),
        ("empty scope", Scope::default()),
    ])
    .collect();
    assert_parity(&cases, &dataset(true));
}

#[test]
fn a_text_filter_over_a_dataset_with_no_textual_column_matches_nothing() {
    assert_parity(
        &[
            ("no textual column", text("SPX")),
            ("no textual, empty", text("")),
            (
                "no textual folds a junk cast",
                Scope {
                    text: Some("SPX".into()),
                    expression: Some(parse_expr("strike = 'abc'").unwrap()),
                    ..Scope::default()
                },
            ),
            (
                "no textual keeps a binder error",
                Scope {
                    text: Some("SPX".into()),
                    expression: Some(parse_expr("cur < 5").unwrap()),
                    ..Scope::default()
                },
            ),
        ],
        &dataset(false),
    );
}

#[test]
fn a_scope_with_unresolved_names_is_refused_by_both() {
    let scope = Scope {
        named: vec!["liq".into()],
        ..Scope::default()
    };
    assert_parity(&[("named", scope)], &dataset(true));
}
