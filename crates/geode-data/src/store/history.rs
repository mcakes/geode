//! Keyed bitemporal history storage: one payload table and one coverage
//! table per history dataset. A row is one observation of a key tuple at `ts`;
//! a correction appends a version under a later `received_at`, and the live
//! value of `(key, ts)` is the version with the greatest `received_at`.
//! Retention deletes superseded versions or observations beyond the history
//! window. History datasets have no live/archive pairs or generation IDs.
//!
//! Coverage records which asked key (an exact tuple, or a prefix stored with
//! trailing NULLs) was fetched over which span, whether or not any row came
//! back, so an empty answer is not asked for again. Each append coalesces
//! the coverage of the tuples it touched, one row per disjoint span.
//!
//! Both timestamps are UTC stored as naive `TIMESTAMP`, bound and read as
//! epoch microseconds (`make_timestamp` / `epoch_us`) so session time zones
//! cannot shift them. The ingest writer serializes calls to [`append_history`]
//! because they share the fixed `staging_history` table.

use crate::adapter::HistoryRows;
use crate::store::series::{cutoff, from_micros, merge_spans, micros, missing_spans};
use crate::store::{Store, StoreError};
use chrono::{DateTime, Utc};
use duckdb::Connection;
use duckdb::types::Value;
use geode_core::query::{HistoryRow, HistoryTable, KeySet, Span, normalize_key};
use geode_core::schema::{ColumnSpec, ColumnType, DatasetSpec};
use std::collections::{HashMap, HashSet};

pub const STAGING_TABLE: &str = "staging_history";

/// A half-open `[from, to)` coverage span. Named apart from
/// `geode_core::query::Span`, the request vocabulary, so the two never meet
/// under one name.
pub type CoverSpan = crate::store::series::Span;

pub fn history_table(dataset: &str) -> String {
    format!("{dataset}_history")
}

pub fn history_coverage_table(dataset: &str) -> String {
    format!("{dataset}_history_coverage")
}

fn quote(name: &str) -> String {
    format!("\"{name}\"")
}

/// Create missing payload and coverage tables. Payload columns follow
/// `DatasetSpec::history_columns`: the key columns typed as declared, `ts`,
/// `received_at`, then the value columns as DOUBLE, keyed by
/// `(key…, ts, received_at)`. Coverage repeats the key columns (nullable, so
/// a prefix leaves the deeper ones NULL) with its span and `received_at`, and
/// has no key. Existing tables are not migrated.
// Any change to this DDL bumps `STORE_FORMAT` (`store::stamp`), so a
// collector from another build refuses this store instead of writing
// positionally into a different layout.
pub fn create_history_tables_sql(ds: &DatasetSpec) -> Vec<String> {
    let keys = ds.history_key();
    let values = ds.history_values();
    let mut cols: Vec<String> = keys
        .iter()
        .map(|c| format!("  {} {}", quote(&c.name), c.ty.sql()))
        .collect();
    cols.push("  \"ts\" TIMESTAMP".into());
    cols.push("  \"received_at\" TIMESTAMP".into());
    cols.extend(
        values
            .iter()
            .map(|c| format!("  {} DOUBLE", quote(&c.name))),
    );
    let pk: Vec<String> = keys
        .iter()
        .map(|c| quote(&c.name))
        .chain(["\"ts\"".to_string(), "\"received_at\"".to_string()])
        .collect();
    let mut cov: Vec<String> = keys
        .iter()
        .map(|c| format!("  {} {}", quote(&c.name), c.ty.sql()))
        .collect();
    cov.extend([
        "  \"from_ts\" TIMESTAMP".to_string(),
        "  \"to_ts\" TIMESTAMP".to_string(),
        "  \"received_at\" TIMESTAMP".to_string(),
    ]);
    vec![
        format!(
            "CREATE TABLE IF NOT EXISTS {} (\n{},\n  PRIMARY KEY ({})\n);",
            history_table(&ds.name),
            cols.join(",\n"),
            pk.join(", ")
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS {} (\n{}\n);",
            history_coverage_table(&ds.name),
            cov.join(",\n")
        ),
    ]
}

/// One asked key (prefix or exact, as the request named it) and the span
/// its answer covers. A `Prefix` tuple shorter than the key is stored with
/// trailing NULLs; lookups treat it as covering every deeper tuple.
#[derive(Debug, Clone, PartialEq)]
pub struct Covered {
    pub asked: Vec<String>,
    pub span: CoverSpan,
}

pub struct HistoryAppendRequest<'a> {
    pub dataset: &'a DatasetSpec,
    /// The source that answered. The tables are per dataset and do not
    /// record it.
    pub source: &'a str,
    pub rows: &'a HistoryRows,
    /// Every asked key this fetch answered, recorded whether or not any row
    /// was new: an empty answer is not asked for again.
    pub covered: Vec<Covered>,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct HistoryAppended {
    pub appended: usize,
    /// Payload rows removed by retention across the dataset in the same
    /// transaction; coverage deletions are excluded from this count.
    pub swept: usize,
}

fn sql_err(statement: &str) -> impl FnOnce(duckdb::Error) -> StoreError + '_ {
    move |source| StoreError::Sql {
        statement: statement.to_string(),
        source,
    }
}

/// The rows' key columns in canonical spelling, column-major. Refuses a
/// shape that does not match the dataset or a key its type cannot read, so a
/// bad answer leaves the store exactly as it was, coverage included. The
/// checks and the mapping are `HistoryRows`' own, so the store and the
/// adapter boundary agree on what a valid answer is.
fn normalized_keys(
    keys: &[&ColumnSpec],
    n_values: usize,
    rows: &HistoryRows,
) -> Result<Vec<Vec<String>>, StoreError> {
    rows.check_shape(keys.len(), n_values)
        .map_err(StoreError::History)?;
    let types: Vec<ColumnType> = keys.iter().map(|c| c.ty).collect();
    rows.normalized_key_columns(&types)
        .map_err(StoreError::History)
}

/// Each covered key padded to the key's arity with NULLs, values in
/// canonical spelling, so `0.50` asked is `0.5` covered.
fn normalized_covered(
    keys: &[&ColumnSpec],
    covered: &[Covered],
) -> Result<Vec<Vec<Option<String>>>, StoreError> {
    covered
        .iter()
        .map(|c| normalized_tuple(keys, &c.asked, "covered"))
        .collect()
}

/// One asked tuple padded to the key's arity with NULLs, values in canonical
/// spelling. Refuses a tuple longer than the key or a value its column type
/// cannot read; `what` names the tuple's role in the message.
fn normalized_tuple(
    keys: &[&ColumnSpec],
    asked: &[String],
    what: &str,
) -> Result<Vec<Option<String>>, StoreError> {
    if asked.len() > keys.len() {
        return Err(StoreError::History(format!(
            "{what} key ({}) is longer than the {}-column key",
            asked.join(" / "),
            keys.len()
        )));
    }
    keys.iter()
        .enumerate()
        .map(|(i, spec)| match asked.get(i) {
            Some(v) => normalize_key(v, spec.ty)
                .map(Some)
                .map_err(|e| StoreError::History(format!("{what} key '{}': {e}", spec.name))),
            None => Ok(None),
        })
        .collect()
}

/// Spans recorded for `asked` or for any shorter prefix of it: a coverage
/// row whose non-NULL leading columns equal the asked tuple's leading values
/// covers it. A row deeper than a prefix ask (an exact tuple under it) does
/// not cover the prefix, or a fetch of one member would make the whole
/// prefix look fetched. Merged and ordered.
pub fn covered_spans(
    conn: &Connection,
    ds: &DatasetSpec,
    asked: &[String],
) -> Result<Vec<CoverSpan>, StoreError> {
    let keys = ds.history_key();
    let tuple = normalized_tuple(&keys, asked, "asked")?;
    let predicate = keys
        .iter()
        .zip(&tuple)
        .map(|(c, v)| match v {
            Some(_) => format!(
                "({0} IS NULL OR {0} = CAST(? AS {1}))",
                quote(&c.name),
                c.ty.sql()
            ),
            None => format!("{} IS NULL", quote(&c.name)),
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    let predicate = if predicate.is_empty() {
        "true".to_string()
    } else {
        predicate
    };
    let sql = format!(
        "select epoch_us(from_ts), epoch_us(to_ts) from {} where {predicate} order by from_ts",
        history_coverage_table(&ds.name)
    );
    let params: Vec<Value> = tuple.into_iter().flatten().map(Value::Text).collect();
    let mut stmt = conn.prepare(&sql).map_err(sql_err(&sql))?;
    let mut spans: Vec<CoverSpan> = stmt
        .query_map(duckdb::params_from_iter(params), |r| {
            Ok((from_micros(r.get(0)?), from_micros(r.get(1)?)))
        })
        .map_err(sql_err(&sql))?
        .collect::<Result<_, _>>()
        .map_err(sql_err(&sql))?;
    merge_spans(&mut spans);
    Ok(spans)
}

/// One asked tuple and the parts of a requested span its coverage lacks.
pub type KeyGaps = (Vec<String>, Vec<CoverSpan>);

/// For each asked tuple, the parts of `requested` its coverage lacks
/// (`series::missing_spans`), keyed by tuple as asked, in input order. A
/// tuple's coverage is what `covered_spans` reads for it: rows equal to it
/// or a prefix of it, never a row deeper than a prefix ask. Refuses a tuple
/// longer than the key, or a value its column cannot read, before any SQL.
///
/// One statement whatever the tuple count (`coverage_batch_sql`); each
/// tuple's rows are then picked from its result.
pub fn missing_by_key(
    conn: &Connection,
    ds: &DatasetSpec,
    asked: &[Vec<String>],
    requested: CoverSpan,
) -> Result<Vec<KeyGaps>, StoreError> {
    let keys = ds.history_key();
    let tuples: Vec<Vec<Option<String>>> = asked
        .iter()
        .map(|tuple| normalized_tuple(&keys, tuple, "asked"))
        .collect::<Result<_, _>>()?;
    if tuples.is_empty() {
        return Ok(Vec::new());
    }
    let n = keys.len();
    let select = coverage_batch_sql(&ds.name, &keys, tuples.len());
    // A cell past a short tuple's end binds NULL, which the join reads as
    // "this column must be NULL in the row", as `covered_spans` does.
    let params: Vec<Value> = tuples
        .iter()
        .flatten()
        .map(|v| v.clone().map_or(Value::Null, Value::Text))
        .collect();
    // Key columns read back in the spelling `normalized_tuple` produces, so
    // a stored key compares equal to the asked one.
    let read_key = |r: &duckdb::Row, i: usize| -> duckdb::Result<Option<String>> {
        match keys[i].ty {
            ColumnType::F64 => r
                .get::<_, Option<f64>>(i)
                .map(|f| f.map(|f| format!("{f}"))),
            _ => r.get::<_, Option<String>>(i),
        }
    };
    let mut stmt = conn.prepare(&select).map_err(sql_err(&select))?;
    let rows: Vec<(Vec<Option<String>>, i64, i64)> = stmt
        .query_map(duckdb::params_from_iter(params), |r| {
            let key = (0..n)
                .map(|i| read_key(r, i))
                .collect::<Result<Vec<_>, _>>()?;
            Ok((key, r.get(n)?, r.get(n + 1)?))
        })
        .map_err(sql_err(&select))?
        .collect::<Result<_, _>>()
        .map_err(sql_err(&select))?;
    // Per asked tuple: the rows that apply to it — each column NULL in the
    // row or equal to the tuple's value there — merged, then the parts of
    // `requested` they leave uncovered.
    Ok(asked
        .iter()
        .zip(&tuples)
        .map(|(tuple, normalized)| {
            let mut held: Vec<CoverSpan> = rows
                .iter()
                .filter(|(key, _, _)| {
                    key.iter()
                        .zip(normalized)
                        .all(|(k, v)| k.is_none() || k == v)
                })
                .map(|&(_, from, to)| (from_micros(from), from_micros(to)))
                .collect();
            merge_spans(&mut held);
            (tuple.clone(), missing_spans(requested, &held))
        })
        .collect())
}

/// The statement `missing_by_key` runs: every coverage row of `dataset`
/// that applies to any of `n_tuples` asked tuples, joined on a VALUES list
/// of them (one text cell per key column, NULL past a short tuple's end) so
/// the predicate's depth does not grow with the tuple count. A row applies
/// when each key column is NULL in it or equal to the asked value; a NULL
/// asked value compares unknown, leaving only the NULL column, which is
/// `covered_spans`' predicate for a prefix ask. Distinct, since one row
/// can apply to many tuples.
fn coverage_batch_sql(dataset: &str, keys: &[&ColumnSpec], n_tuples: usize) -> String {
    let key_list = joined(keys, |c| format!("c.{}", quote(&c.name)), ", ");
    let row = format!("({})", vec!["CAST(? AS VARCHAR)"; keys.len()].join(", "));
    let asked_rows = vec![row.as_str(); n_tuples].join(", ");
    let asked_cols = (0..keys.len())
        .map(|i| format!("a{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let on = keys
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let (col, ty) = (quote(&c.name), c.ty.sql());
            format!("(c.{col} IS NULL OR c.{col} = CAST(asked.a{i} AS {ty}))")
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    format!(
        "select distinct {key_list}, epoch_us(c.from_ts), epoch_us(c.to_ts) from {} c \
         join (values {asked_rows}) as asked({asked_cols}) on {on}",
        history_coverage_table(dataset)
    )
}

/// The asked tuples the cache cannot answer `At(t)` for. A tuple is
/// answerable when it names every key column, `t` lies inside one of its
/// merged coverage spans `[a, b)` (rows recorded for it or for a prefix of
/// it, trailing NULLs), and the cache holds a row of that exact tuple with
/// `a <= ts <= t`. Coverage alone is not enough: a span can hold `t` while
/// the tuple's last observation at or before `t` lies before the span, never
/// fetched, and answering from the cache would serve a stale value as
/// current.
///
/// A tuple naming fewer (or more) key columns than the key never answers
/// locally: a prefix names members the cache cannot enumerate, so one
/// missing member would be invisible. One such tuple makes every asked tuple
/// uncovered, returned as asked with repeats removed. Otherwise the result
/// is normalized, in asked order, repeats removed.
///
/// Two statements whatever the tuple count: every coverage row that applies
/// to any asked tuple, then every covered tuple's last observation at or
/// before `t`.
pub fn uncovered_at(
    conn: &Connection,
    ds: &DatasetSpec,
    asked: &[Vec<String>],
    t: DateTime<Utc>,
) -> Result<Vec<Vec<String>>, StoreError> {
    let keys = ds.history_key();
    if asked.iter().any(|tuple| tuple.len() != keys.len()) {
        let mut all: Vec<Vec<String>> = Vec::with_capacity(asked.len());
        for tuple in asked {
            if !all.contains(tuple) {
                all.push(tuple.clone());
            }
        }
        return Ok(all);
    }
    let mut tuples: Vec<Vec<String>> = Vec::with_capacity(asked.len());
    let mut seen: HashSet<Vec<String>> = HashSet::with_capacity(asked.len());
    for tuple in asked {
        let normalized: Vec<String> = normalized_tuple(&keys, tuple, "asked")?
            .into_iter()
            .flatten()
            .collect();
        if seen.insert(normalized.clone()) {
            tuples.push(normalized);
        }
    }
    if tuples.is_empty() {
        return Ok(tuples);
    }
    let n = keys.len();
    let key_list = joined(&keys, |c| quote(&c.name), ", ");
    // Key columns read back in the spelling `normalized_tuple` produces, so
    // a stored key compares equal to the asked one.
    let read_key = |r: &duckdb::Row, i: usize| -> duckdb::Result<Option<String>> {
        match keys[i].ty {
            ColumnType::F64 => r
                .get::<_, Option<f64>>(i)
                .map(|f| f.map(|f| format!("{f}"))),
            _ => r.get::<_, Option<String>>(i),
        }
    };
    // 1) Every coverage row that applies to an asked tuple: equal to it, or
    //    a prefix of it (trailing NULLs), the predicate `covered_spans` uses
    //    for a full-arity tuple.
    let one = joined(
        &keys,
        |c| {
            let (col, ty) = (quote(&c.name), c.ty.sql());
            format!("({col} = CAST(? AS {ty}) OR {col} IS NULL)")
        },
        " AND ",
    );
    let predicate = vec![format!("({one})"); tuples.len()].join(" OR ");
    let select = format!(
        "select {key_list}, epoch_us(from_ts), epoch_us(to_ts) from {} where {predicate}",
        history_coverage_table(&ds.name)
    );
    let params: Vec<Value> = tuples.iter().flatten().cloned().map(Value::Text).collect();
    let mut stmt = conn.prepare(&select).map_err(sql_err(&select))?;
    let rows: Vec<(Vec<Option<String>>, i64, i64)> = stmt
        .query_map(duckdb::params_from_iter(params), |r| {
            let key = (0..n)
                .map(|i| read_key(r, i))
                .collect::<Result<Vec<_>, _>>()?;
            Ok((key, r.get(n)?, r.get(n + 1)?))
        })
        .map_err(sql_err(&select))?
        .collect::<Result<_, _>>()
        .map_err(sql_err(&select))?;
    // Per tuple: the merged spans of the rows that apply to it, and the
    // start of the one holding `t`.
    let mut uncovered: Vec<Vec<String>> = Vec::new();
    let mut starts: Vec<(&Vec<String>, DateTime<Utc>)> = Vec::with_capacity(tuples.len());
    for tuple in &tuples {
        let mut spans: Vec<CoverSpan> = rows
            .iter()
            .filter(|(key, _, _)| {
                key.iter()
                    .zip(tuple)
                    .all(|(k, v)| k.as_ref().is_none_or(|k| k == v))
            })
            .map(|&(_, from, to)| (from_micros(from), from_micros(to)))
            .collect();
        merge_spans(&mut spans);
        match spans.iter().find(|(a, b)| *a <= t && t < *b) {
            Some(&(a, _)) => starts.push((tuple, a)),
            None => uncovered.push(tuple.clone()), // no span of this tuple holds t
        }
    }
    if !starts.is_empty() {
        // 2) Every covered tuple's last observation at or before t.
        let exact = joined(
            &keys,
            |c| format!("{} = CAST(? AS {})", quote(&c.name), c.ty.sql()),
            " AND ",
        );
        let predicate = vec![format!("({exact})"); starts.len()].join(" OR ");
        let latest = format!(
            "select {key_list}, max(epoch_us(ts)) from {} \
             where ({predicate}) and epoch_us(ts) <= ? group by {key_list}",
            history_table(&ds.name)
        );
        let mut params: Vec<Value> = starts
            .iter()
            .flat_map(|(tuple, _)| tuple.iter().cloned().map(Value::Text))
            .collect();
        params.push(Value::BigInt(micros(t)));
        let mut stmt = conn.prepare(&latest).map_err(sql_err(&latest))?;
        let observed: HashMap<Vec<String>, i64> = stmt
            .query_map(duckdb::params_from_iter(params), |r| {
                let key = (0..n)
                    .map(|i| read_key(r, i).map(Option::unwrap_or_default))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((key, r.get(n)?))
            })
            .map_err(sql_err(&latest))?
            .collect::<Result<_, _>>()
            .map_err(sql_err(&latest))?;
        for (tuple, a) in &starts {
            let answered = observed
                .get_key_value(*tuple)
                .is_some_and(|(_, last)| from_micros(*last) >= *a); // a row at or before t, inside the span
            if !answered {
                uncovered.push((*tuple).clone());
            }
        }
        // Asked order, for a stable call shape.
        uncovered.sort_by_key(|u| tuples.iter().position(|t| t == u));
    }
    Ok(uncovered)
}

/// Whether the cache can answer `At(t)` for every asked tuple:
/// `uncovered_at` names none. A tuple of the wrong arity makes this
/// `Ok(false)`, never an error.
pub fn covers_instant(
    conn: &Connection,
    ds: &DatasetSpec,
    asked: &[Vec<String>],
    t: DateTime<Utc>,
) -> Result<bool, StoreError> {
    Ok(uncovered_at(conn, ds, asked, t)?.is_empty())
}

/// Live read of the cached rows for `keys` under `span`: the greatest
/// `received_at` per (key, ts); `Latest` = the greatest ts per key;
/// `At(t)` = the greatest ts <= t per key; `Between` = every ts in
/// `[from, to)`. Rows ordered by key columns then ts. Keys are matched by
/// prefix or exactly as the set says.
///
/// Serves what the cache holds: whether that is enough to answer a request
/// is the caller's decision. `Between`'s `step` is a hint to the source and
/// does not thin cached rows.
pub fn read_history(
    conn: &Connection,
    ds: &DatasetSpec,
    keys: &KeySet,
    span: Span,
) -> Result<HistoryTable, StoreError> {
    let key_cols = ds.history_key();
    let values = ds.history_values();
    // A wrong arity would bind values to the wrong key columns.
    keys.validate(key_cols.len()).map_err(StoreError::History)?;
    // Asked values in canonical spelling, so `0.50` reads the `0.5` key.
    let tuples = keys
        .tuples()
        .iter()
        .map(|t| normalized_tuple(&key_cols, t, "asked"))
        .collect::<Result<Vec<_>, _>>()?;

    let mut params: Vec<Value> = Vec::new();
    let predicate = tuples
        .iter()
        .map(|tuple| {
            let matched = key_cols
                .iter()
                .zip(tuple)
                .filter_map(|(c, v)| {
                    let v = v.as_ref()?;
                    params.push(Value::Text(v.clone()));
                    Some(format!("{} = CAST(? AS {})", quote(&c.name), c.ty.sql()))
                })
                .collect::<Vec<_>>()
                .join(" AND ");
            format!("({matched})")
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    let window = match span {
        Span::Latest => String::new(),
        Span::At(t) => {
            params.push(Value::BigInt(micros(t)));
            " and ts <= make_timestamp(?)".to_string()
        }
        Span::Between { from, to, .. } => {
            params.extend([Value::BigInt(micros(from)), Value::BigInt(micros(to))]);
            " and ts >= make_timestamp(?) and ts < make_timestamp(?)".to_string()
        }
    };

    let key_list = joined(&key_cols, |c| quote(&c.name), ", ");
    let value_list = joined(&values, |c| quote(&c.name), ", ");
    let live_values = joined(
        &values,
        |c| format!("arg_max({0}, received_at) as {0}", quote(&c.name)),
        ", ",
    );
    let projection = format!("{key_list}, epoch_us(ts), {value_list}");
    let answer = match span {
        Span::Between { .. } => format!("select {projection} from live"),
        Span::Latest | Span::At(_) => format!(
            "select {projection} from (
                 select *, row_number() over (partition by {key_list} order by ts desc) as rn
                 from live
             ) where rn = 1"
        ),
    };
    let sql = format!(
        "with live as (
             select {key_list}, ts, {live_values} from {}
             where ({predicate}){window}
             group by {key_list}, ts
         )
         {answer} order by {key_list}, ts",
        history_table(&ds.name)
    );

    let n_keys = key_cols.len();
    let n_values = values.len();
    let mut stmt = conn.prepare(&sql).map_err(sql_err(&sql))?;
    let rows = stmt
        .query_map(duckdb::params_from_iter(params), |r| {
            let key = key_cols
                .iter()
                .enumerate()
                .map(|(i, c)| match c.ty {
                    // Read back in the spelling `normalize_key` writes.
                    ColumnType::F64 => r.get::<_, f64>(i).map(|f| format!("{f}")),
                    _ => r.get::<_, String>(i),
                })
                .collect::<Result<Vec<_>, _>>()?;
            let ts = from_micros(r.get(n_keys)?);
            let values = (0..n_values)
                .map(|v| r.get::<_, f64>(n_keys + 1 + v))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(HistoryRow { key, ts, values })
        })
        .map_err(sql_err(&sql))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err(&sql))?;
    Ok(HistoryTable {
        key_columns: ds.key.clone(),
        value_columns: values.iter().map(|c| c.name.clone()).collect(),
        rows,
    })
}

pub fn append_history(
    store: &Store,
    req: &HistoryAppendRequest,
) -> Result<HistoryAppended, StoreError> {
    let keys = req.dataset.history_key();
    let values = req.dataset.history_values();
    // Before anything is written: a malformed answer leaves the store
    // exactly as it was, coverage included.
    let key_cols = normalized_keys(&keys, values.len(), req.rows)?;
    let covers = normalized_covered(&keys, &req.covered)?;
    let conn = store.writer();

    // Stage keys as canonical text and ts as BIGINT micros: the insert
    // casts each key to its declared type, and a micros column cannot be
    // bent by a session time zone.
    let mut staged = vec!["  \"ts_us\" BIGINT".to_string()];
    staged.extend(keys.iter().map(|c| format!("  {} VARCHAR", quote(&c.name))));
    staged.extend(
        values
            .iter()
            .map(|c| format!("  {} DOUBLE", quote(&c.name))),
    );
    let create = format!(
        "create or replace table {STAGING_TABLE} (\n{}\n)",
        staged.join(",\n")
    );
    conn.execute_batch(&create).map_err(sql_err(&create))?;
    {
        let mut app = conn
            .appender(STAGING_TABLE)
            .map_err(sql_err("appender on staging_history"))?;
        for i in 0..req.rows.len() {
            let mut row = Vec::with_capacity(1 + keys.len() + values.len());
            row.push(Value::BigInt(micros(req.rows.ts[i])));
            row.extend(key_cols.iter().map(|c| Value::Text(c[i].clone())));
            row.extend(req.rows.values.iter().map(|c| Value::Double(c[i])));
            app.append_row(duckdb::appender_params_from_iter(row))
                .map_err(sql_err("append row into staging_history"))?;
        }
        app.flush().map_err(sql_err("flush staging_history"))?;
    }

    conn.execute_batch("begin;").map_err(sql_err("begin"))?;
    match append_in_transaction(conn, req, &keys, &values, &covers) {
        Ok(out) => {
            conn.execute_batch("commit;").map_err(sql_err("commit"))?;
            Ok(out)
        }
        Err(e) => {
            let _ = conn.execute_batch("rollback;");
            Err(e)
        }
    }
}

fn joined(cols: &[&ColumnSpec], f: impl Fn(&ColumnSpec) -> String, sep: &str) -> String {
    cols.iter().map(|c| f(c)).collect::<Vec<_>>().join(sep)
}

fn append_in_transaction(
    conn: &Connection,
    req: &HistoryAppendRequest,
    keys: &[&ColumnSpec],
    values: &[&ColumnSpec],
    covers: &[Vec<Option<String>>],
) -> Result<HistoryAppended, StoreError> {
    let table = history_table(&req.dataset.name);
    let coverage = history_coverage_table(&req.dataset.name);
    let received = micros(req.received_at);
    let key_list = joined(keys, |c| quote(&c.name), ", ");
    let value_list = joined(values, |c| quote(&c.name), ", ");

    // Drop a staged row equal in every value column to the live version of
    // its (key, ts), so an overlapping refetch grows nothing. Live is the
    // greatest received_at per (key, ts), expressed inline; the match is on
    // the whole key, or an equal value under another key would be dropped.
    let live_values = joined(
        values,
        |c| format!("arg_max({0}, received_at) as {0}", quote(&c.name)),
        ", ",
    );
    let key_match = joined(
        keys,
        |c| format!("live.{0} = CAST(s.{0} AS {1})", quote(&c.name), c.ty.sql()),
        " and ",
    );
    let value_match = joined(
        values,
        |c| format!("live.{0} = s.{0}", quote(&c.name)),
        " and ",
    );
    // Live versions of the staged keys only: the dedupe compares each
    // staged row with the live row of its own (key, ts), so rows under keys
    // this append does not touch need not be grouped at all. The filter is
    // on the key, never on received_at, so every version of a staged
    // (key, ts) is still grouped and live stays its newest. Without the
    // restriction every append grouped the whole payload table.
    let staged_match = joined(
        keys,
        |c| format!("t.{0} = CAST(s2.{0} AS {1})", quote(&c.name), c.ty.sql()),
        " and ",
    );
    let dedupe = format!(
        "delete from {STAGING_TABLE} s where exists (
             select 1 from (
                 select {key_list}, ts, {live_values} from {table} t
                 where exists (select 1 from {STAGING_TABLE} s2 where {staged_match})
                 group by {key_list}, ts
             ) live
             where {key_match} and live.ts = make_timestamp(s.ts_us) and {value_match}
         )"
    );
    conn.execute(&dedupe, []).map_err(sql_err(&dedupe))?;

    // Insert what remains under one received_at, each key cast from its
    // canonical text to the declared type.
    let casts = joined(
        keys,
        |c| format!("CAST({} AS {})", quote(&c.name), c.ty.sql()),
        ", ",
    );
    let insert = format!(
        "insert into {table} ({key_list}, ts, received_at, {value_list})
         select {casts}, make_timestamp(ts_us), make_timestamp(?), {value_list} \
         from {STAGING_TABLE}"
    );
    let appended = conn
        .execute(&insert, duckdb::params![received])
        .map_err(sql_err(&insert))?;

    // Record coverage even when deduplication leaves no new rows.
    let cover_casts = joined(keys, |c| format!("CAST(? AS {})", c.ty.sql()), ", ");
    let cover = format!(
        "insert into {coverage} ({key_list}, from_ts, to_ts, received_at)
         values ({cover_casts}, make_timestamp(?), make_timestamp(?), make_timestamp(?))"
    );
    for (asked, c) in covers.iter().zip(&req.covered) {
        let mut params: Vec<Value> = asked
            .iter()
            .map(|v| v.clone().map_or(Value::Null, Value::Text))
            .collect();
        params.extend([
            Value::BigInt(micros(c.span.0)),
            Value::BigInt(micros(c.span.1)),
            Value::BigInt(received),
        ]);
        conn.execute(&cover, duckdb::params_from_iter(params))
            .map_err(sql_err(&cover))?;
    }
    coalesce_coverage(conn, &coverage, keys, covers)?;

    // Bound the dataset's history in the same transaction as rows and
    // coverage.
    let swept = sweep_history(conn, req.dataset, req.received_at)?;
    Ok(HistoryAppended { appended, swept })
}

/// Rewrite the coverage rows of every tuple in `touched` (exactly that
/// tuple, trailing NULLs included, never a deeper or shallower one) as its
/// merged spans, each under the newest `received_at` among the rows it
/// replaces. Without it a refreshing board adds a point row per tuple per
/// ask, and every lookup scans them all on the service loop. Lookups merge
/// a tuple's spans with its prefixes' anyway, so they answer the same.
fn coalesce_coverage(
    conn: &Connection,
    coverage: &str,
    keys: &[&ColumnSpec],
    touched: &[Vec<Option<String>>],
) -> Result<(), StoreError> {
    let same = joined(
        keys,
        |c| {
            format!(
                "{} IS NOT DISTINCT FROM CAST(? AS {})",
                quote(&c.name),
                c.ty.sql()
            )
        },
        " and ",
    );
    let select = format!(
        "select epoch_us(from_ts), epoch_us(to_ts), epoch_us(received_at) \
         from {coverage} where {same}"
    );
    let delete = format!("delete from {coverage} where {same}");
    let key_list = joined(keys, |c| quote(&c.name), ", ");
    let cover_casts = joined(keys, |c| format!("CAST(? AS {})", c.ty.sql()), ", ");
    let insert = format!(
        "insert into {coverage} ({key_list}, from_ts, to_ts, received_at)
         values ({cover_casts}, make_timestamp(?), make_timestamp(?), make_timestamp(?))"
    );
    // One prepare per append, not one per touched tuple.
    let mut stmt = conn.prepare(&select).map_err(sql_err(&select))?;
    let mut done: Vec<&Vec<Option<String>>> = Vec::new();
    for tuple in touched {
        if done.contains(&tuple) {
            continue;
        }
        done.push(tuple);
        let params: Vec<Value> = tuple
            .iter()
            .map(|v| v.clone().map_or(Value::Null, Value::Text))
            .collect();
        let held: Vec<(i64, i64, i64)> = stmt
            .query_map(duckdb::params_from_iter(params.clone()), |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(sql_err(&select))?
            .collect::<Result<_, _>>()
            .map_err(sql_err(&select))?;
        let Some(newest) = held.iter().map(|h| h.2).max() else {
            continue;
        };
        let mut spans: Vec<CoverSpan> = held
            .iter()
            .map(|&(f, t, _)| (from_micros(f), from_micros(t)))
            .collect();
        merge_spans(&mut spans);
        conn.execute(&delete, duckdb::params_from_iter(params.clone()))
            .map_err(sql_err(&delete))?;
        for (from, to) in spans {
            let mut row = params.clone();
            row.extend([
                Value::BigInt(micros(from)),
                Value::BigInt(micros(to)),
                Value::BigInt(newest),
            ]);
            conn.execute(&insert, duckdb::params_from_iter(row))
                .map_err(sql_err(&insert))?;
        }
    }
    Ok(())
}

/// Apply retention across a history dataset. `append_history` calls this
/// inside its transaction with the append's `received_at` as `now`; other
/// callers must supply any required transaction.
///
/// `retention` removes superseded versions whose `received_at` precedes the
/// cutoff; the live version of every `(key, ts)` survives regardless of age.
/// `history` removes payload rows with `ts` before its cutoff and coverage
/// spans ending at or before it. Returns the number of deleted payload rows.
///
/// An unrepresentable cutoff skips only that window's deletions
/// (`series::cutoff`): wrapping it could produce a future cutoff and delete
/// every row.
pub fn sweep_history(
    conn: &Connection,
    ds: &DatasetSpec,
    now: DateTime<Utc>,
) -> Result<usize, StoreError> {
    let Some(policy) = ds.series_retention else {
        return Ok(0);
    };
    let table = history_table(&ds.name);
    let mut swept = 0;
    if let Some(window) = policy.retention
        && let Some(cutoff) = cutoff(now, window)
    {
        let same_key = joined(
            &ds.history_key(),
            |c| format!("n.{0} = t.{0}", quote(&c.name)),
            " and ",
        );
        let sql = format!(
            "delete from {table} t where epoch_us(received_at) < ? \
             and exists (select 1 from {table} n where {same_key} \
                 and n.ts = t.ts and n.received_at > t.received_at)"
        );
        swept += conn
            .execute(&sql, duckdb::params![cutoff])
            .map_err(sql_err(&sql))?;
    }
    if let Some(window) = policy.history
        && let Some(cutoff) = cutoff(now, window)
    {
        let sql = format!("delete from {table} where epoch_us(ts) < ?");
        swept += conn
            .execute(&sql, duckdb::params![cutoff])
            .map_err(sql_err(&sql))?;
        let sql = format!(
            "delete from {} where epoch_us(to_ts) <= ?",
            history_coverage_table(&ds.name)
        );
        conn.execute(&sql, duckdb::params![cutoff])
            .map_err(sql_err(&sql))?;
    }
    Ok(swept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::store::catalog::Catalog;
    use crate::store::ddl::tests_support::{history_dataset, ts};
    use crate::store::series::micros;
    use proptest::prelude::*;

    fn fixture() -> (tempfile::TempDir, Store, DatasetSpec) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = history_dataset();
        store.apply_schema(&ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        (dir, store, ds)
    }

    /// `days` daily observations of `("SPX", "3M", "0.5")` at 21:00 UTC from
    /// `day0`, `vol = base + i`, `forward = 7650.0`.
    fn surface_rows(day0: &str, days: usize, base: f64) -> HistoryRows {
        let start = ts(&format!("{day0}T21:00:00Z"));
        HistoryRows {
            ts: (0..days)
                .map(|i| start + chrono::Duration::days(i as i64))
                .collect(),
            keys: vec![
                vec!["SPX".into(); days],
                vec!["3M".into(); days],
                vec!["0.5".into(); days],
            ],
            values: vec![
                (0..days).map(|i| base + i as f64).collect(),
                vec![7650.0; days],
            ],
        }
    }

    type Row = (i64, i64, String, String, f64, f64, f64);

    /// Every stored version, oldest observation first: the raw table, not
    /// the live view.
    fn all_rows(store: &Store) -> Vec<Row> {
        let mut stmt = store
            .writer()
            .prepare(
                "select epoch_us(ts), epoch_us(received_at), underlying_ref, tenor, delta, vol, \
                 forward from vol_surface_history order by ts, received_at",
            )
            .unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    fn coverage_count(store: &Store) -> i64 {
        store
            .writer()
            .query_row(
                "select count(*) from vol_surface_history_coverage",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn spx(from: &str, to: &str) -> Vec<Covered> {
        vec![Covered {
            asked: vec!["SPX".into()],
            span: (ts(from), ts(to)),
        }]
    }

    #[test]
    fn the_ddl_lists_key_columns_then_ts_received_at_then_values() {
        let ds = history_dataset();
        let sql = create_history_tables_sql(&ds);
        assert_eq!(sql.len(), 2);
        assert!(sql[0].contains("vol_surface_history ("));
        assert!(sql[0].contains("\"underlying_ref\" VARCHAR"));
        assert!(sql[0].contains("\"delta\" DOUBLE"));
        assert!(sql[0].contains(
            "PRIMARY KEY (\"underlying_ref\", \"tenor\", \"delta\", \"ts\", \"received_at\")"
        ));
        let mut last = 0usize;
        for needle in [
            "\"underlying_ref\" VARCHAR",
            "\"tenor\" VARCHAR",
            "\"delta\" DOUBLE",
            "\"ts\" TIMESTAMP",
            "\"received_at\" TIMESTAMP",
            "\"vol\" DOUBLE",
            "\"forward\" DOUBLE",
        ] {
            let at = sql[0]
                .find(needle)
                .unwrap_or_else(|| panic!("{needle:?} missing from {}", sql[0]));
            assert!(at > last, "{needle:?} out of order in {}", sql[0]);
            last = at;
        }
        assert!(sql[1].contains("vol_surface_history_coverage ("));
        assert!(sql[1].contains("\"from_ts\" TIMESTAMP"));
        assert!(
            !sql[1].contains("PRIMARY KEY"),
            "coverage has no key: a tuple holds one row per disjoint span"
        );
    }

    #[test]
    fn a_first_append_lands_rows_normalizes_f64_keys_and_records_coverage() {
        let (_d, store, ds) = fixture();
        let mut rows = surface_rows("2026-01-05", 3, 15.0);
        rows.keys[2] = vec!["0.50".into(); 3]; // the source's spelling
        let out = append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &rows,
                covered: spx("2026-01-05T00:00:00Z", "2026-01-08T00:00:00Z"),
                received_at: ts("2026-01-08T09:00:00Z"),
            },
        )
        .unwrap();
        assert_eq!(
            out,
            HistoryAppended {
                appended: 3,
                swept: 0
            }
        );
        let got = all_rows(&store);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].4, 0.5, "the f64 key is stored as a number");
        assert_eq!(
            got[0],
            (
                micros(ts("2026-01-05T21:00:00Z")),
                micros(ts("2026-01-08T09:00:00Z")),
                "SPX".to_string(),
                "3M".to_string(),
                0.5,
                15.0,
                7650.0
            )
        );
        let cov: (Option<String>, Option<String>, Option<f64>, i64, i64) = store
            .writer()
            .query_row(
                "select underlying_ref, tenor, delta, epoch_us(from_ts), epoch_us(to_ts) \
                 from vol_surface_history_coverage",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(cov.0.as_deref(), Some("SPX"));
        assert_eq!(
            (cov.1, cov.2),
            (None, None),
            "a prefix is stored with trailing NULLs"
        );
        assert_eq!(
            (cov.3, cov.4),
            (
                micros(ts("2026-01-05T00:00:00Z")),
                micros(ts("2026-01-08T00:00:00Z"))
            )
        );
    }

    #[test]
    fn an_exact_covered_tuple_is_stored_typed_and_normalized() {
        let (_d, store, ds) = fixture();
        let rows = surface_rows("2026-01-05", 1, 15.0);
        append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &rows,
                covered: vec![Covered {
                    asked: vec!["SPX".into(), "3M".into(), "0.50".into()],
                    span: (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
                }],
                received_at: ts("2026-01-06T09:00:00Z"),
            },
        )
        .unwrap();
        let cov: (String, String, f64) = store
            .writer()
            .query_row(
                "select underlying_ref, tenor, delta from vol_surface_history_coverage",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(cov, ("SPX".to_string(), "3M".to_string(), 0.5));
    }

    #[test]
    fn a_covered_tuple_longer_than_the_key_is_refused_before_any_sql() {
        let (_d, store, ds) = fixture();
        let rows = surface_rows("2026-01-05", 1, 15.0);
        let err = append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &rows,
                covered: vec![Covered {
                    asked: vec!["SPX".into(), "3M".into(), "0.5".into(), "x".into()],
                    span: (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
                }],
                received_at: ts("2026-01-06T09:00:00Z"),
            },
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::History(_)), "{err}");
        assert!(all_rows(&store).is_empty());
        assert_eq!(coverage_count(&store), 0);
    }

    #[test]
    fn an_overlapping_refetch_with_equal_values_appends_nothing_but_records_coverage() {
        let (_d, store, ds) = fixture();
        let rows = surface_rows("2026-01-05", 3, 15.0);
        let req = |at: &str| HistoryAppendRequest {
            dataset: &ds,
            source: "hanweck",
            rows: &rows,
            covered: spx("2026-01-05T00:00:00Z", "2026-01-08T00:00:00Z"),
            received_at: ts(at),
        };
        append_history(&store, &req("2026-01-08T09:00:00Z")).unwrap();
        let again = append_history(&store, &req("2026-01-09T09:00:00Z")).unwrap();
        assert_eq!(again.appended, 0);
        assert_eq!(all_rows(&store).len(), 3);
        assert_eq!(
            coverage_count(&store),
            1,
            "the repeated span is coalesced into the first"
        );
    }

    #[test]
    fn repeated_asks_on_one_tuple_coalesce_to_one_coverage_row_per_disjoint_span() {
        let (_d, store, ds) = fixture();
        let exact: Vec<String> = vec!["SPX".into(), "3M".into(), "0.5".into()];
        let tick = chrono::Duration::microseconds(1);
        let point = |s: &str| (ts(s), ts(s) + tick);
        let empty = HistoryRows {
            ts: Vec::new(),
            keys: vec![Vec::new(); 3],
            values: vec![Vec::new(); 2],
        };
        // A refreshing board: the same Latest point many times, a span
        // touching another, an overlapping one, and one apart from the rest.
        let mut asks: Vec<(Vec<String>, CoverSpan)> = (0..20)
            .map(|_| (exact.clone(), point("2026-01-07T21:00:00Z")))
            .collect();
        asks.extend([
            (
                exact.clone(),
                (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
            ),
            (
                exact.clone(),
                (ts("2026-01-06T00:00:00Z"), ts("2026-01-07T00:00:00Z")),
            ),
            (
                exact.clone(),
                (ts("2026-01-05T12:00:00Z"), ts("2026-01-06T12:00:00Z")),
            ),
            (
                exact.clone(),
                (ts("2026-01-12T00:00:00Z"), ts("2026-01-13T00:00:00Z")),
            ),
            // A prefix is a tuple of its own: never merged into the exact one.
            (
                vec!["SPX".into()],
                (ts("2026-01-08T00:00:00Z"), ts("2026-01-09T00:00:00Z")),
            ),
        ]);
        let mut expected: Vec<CoverSpan> = Vec::new();
        for (i, (asked, span)) in asks.iter().enumerate() {
            append_history(
                &store,
                &HistoryAppendRequest {
                    dataset: &ds,
                    source: "hanweck",
                    rows: &empty,
                    covered: vec![Covered {
                        asked: asked.clone(),
                        span: *span,
                    }],
                    received_at: ts("2026-01-14T09:00:00Z") + chrono::Duration::seconds(i as i64),
                },
            )
            .unwrap();
            // The prefix's span covers the exact tuple too.
            expected.push(*span);
            let mut want = expected.clone();
            merge_spans(&mut want);
            assert_eq!(
                covered_spans(store.writer(), &ds, &exact).unwrap(),
                want,
                "lookups answer the union of everything recorded"
            );
        }
        assert_eq!(
            coverage_count(&store),
            4,
            "the exact tuple's three disjoint spans and the prefix's one"
        );
        let newest: i64 = store
            .writer()
            .query_row(
                "select max(epoch_us(received_at)) from vol_surface_history_coverage \
                 where tenor is not null",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(newest, micros(ts("2026-01-14T09:00:23Z")));
        let merged_received: i64 = store
            .writer()
            .query_row(
                "select epoch_us(received_at) from vol_surface_history_coverage \
                 where tenor is not null and epoch_us(from_ts) = ?",
                [micros(ts("2026-01-05T00:00:00Z"))],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            merged_received,
            micros(ts("2026-01-14T09:00:23Z")),
            "a coalesced row carries the newest received_at"
        );
    }

    #[test]
    fn a_changed_value_in_any_column_is_a_new_version_and_the_old_survives() {
        let (_d, store, ds) = fixture();
        let rows = surface_rows("2026-01-05", 1, 15.0);
        let mut corrected = rows.clone();
        corrected.values[1][0] = 7700.0; // forward changed, vol equal
        let covered = || spx("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z");
        append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &rows,
                covered: covered(),
                received_at: ts("2026-01-06T09:00:00Z"),
            },
        )
        .unwrap();
        let out = append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &corrected,
                covered: covered(),
                received_at: ts("2026-01-07T09:00:00Z"),
            },
        )
        .unwrap();
        assert_eq!(out.appended, 1);
        assert_eq!(all_rows(&store).len(), 2);
    }

    #[test]
    fn a_third_append_of_the_original_values_re_appends_because_live_is_the_correction() {
        let (_d, store, ds) = fixture();
        let original = surface_rows("2026-01-05", 1, 15.0);
        let mut corrected = original.clone();
        corrected.values[0][0] = 15.5;
        for (r, at, appended) in [
            (&original, "2026-01-06T09:00:00Z", 1),
            (&corrected, "2026-01-07T09:00:00Z", 1),
            // Live is now the correction; the original again is a third
            // version, because the dedupe compares against live only.
            (&original, "2026-01-08T09:00:00Z", 1),
        ] {
            let out = append_history(
                &store,
                &HistoryAppendRequest {
                    dataset: &ds,
                    source: "hanweck",
                    rows: r,
                    covered: vec![],
                    received_at: ts(at),
                },
            )
            .unwrap();
            assert_eq!(out.appended, appended, "at {at}");
        }
        let vols: Vec<f64> = all_rows(&store).iter().map(|r| r.5).collect();
        assert_eq!(vols, [15.0, 15.5, 15.0]);
    }

    #[test]
    fn an_equal_value_under_another_key_still_lands() {
        let (_d, store, ds) = fixture();
        let rows = surface_rows("2026-01-05", 1, 15.0);
        let covered = || spx("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z");
        append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &rows,
                covered: covered(),
                received_at: ts("2026-01-06T09:00:00Z"),
            },
        )
        .unwrap();
        // Same ts and values, another delta: the dedupe must match on the
        // whole key, or this row would be dropped as already live. The live
        // 0.5 row is staged again beside it, so the dedupe's live set holds
        // a row the 0.25 one could wrongly match; the repeat itself is
        // deduped.
        let mut other = rows.clone();
        other.ts.push(rows.ts[0]);
        other.keys[0].push("SPX".into());
        other.keys[1].push("3M".into());
        other.keys[2] = vec!["0.25".into(), "0.5".into()];
        other.values[0].push(rows.values[0][0]);
        other.values[1].push(rows.values[1][0]);
        let out = append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &other,
                covered: covered(),
                received_at: ts("2026-01-07T09:00:00Z"),
            },
        )
        .unwrap();
        assert_eq!(out.appended, 1);
        assert_eq!(all_rows(&store).len(), 2);
    }

    #[test]
    fn an_unparsable_f64_key_refuses_the_whole_append_before_any_sql() {
        let (_d, store, ds) = fixture();
        let mut rows = surface_rows("2026-01-05", 2, 15.0);
        rows.keys[2][1] = "atm".into();
        let err = append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &rows,
                covered: vec![],
                received_at: ts("2026-01-06T09:00:00Z"),
            },
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::History(_)), "{err}");
        assert!(all_rows(&store).is_empty());
        assert_eq!(coverage_count(&store), 0, "coverage untouched");
    }

    #[test]
    fn a_short_value_column_is_refused_before_any_sql() {
        let (_d, store, ds) = fixture();
        let mut rows = surface_rows("2026-01-05", 2, 15.0);
        rows.values[1].pop();
        let err = append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &rows,
                covered: spx("2026-01-05T00:00:00Z", "2026-01-07T00:00:00Z"),
                received_at: ts("2026-01-07T09:00:00Z"),
            },
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::History(_)), "{err}");
        assert!(all_rows(&store).is_empty());
        assert_eq!(coverage_count(&store), 0, "coverage untouched");
    }

    #[test]
    fn the_history_window_sweeps_old_rows_and_coverage_and_retention_sweeps_superseded_versions() {
        let (_d, store, ds) = fixture(); // history 400d, retention 7d
        let old = surface_rows("2024-01-08", 1, 10.0);
        append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &old,
                covered: spx("2024-01-08T00:00:00Z", "2024-01-09T00:00:00Z"),
                received_at: ts("2024-01-09T09:00:00Z"),
            },
        )
        .unwrap();
        let recent = surface_rows("2026-01-05", 1, 15.0);
        let mut corrected = recent.clone();
        corrected.values[0][0] = 15.5;
        let cov = || spx("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z");
        // The 2024 row is older than 400d from this `now`: the sweep is
        // dataset-wide, so this append's history window removes it.
        let out = append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &recent,
                covered: cov(),
                received_at: ts("2026-01-06T09:00:00Z"),
            },
        )
        .unwrap();
        assert_eq!(out.appended, 1);
        assert_eq!(out.swept, 1, "the 2024 row by history");
        let n: i64 = store
            .writer()
            .query_row(
                "select count(*) from vol_surface_history_coverage where epoch_us(to_ts) <= ?",
                [micros(ts("2024-12-12T09:00:00Z"))],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "coverage ending before the horizon is gone");
        // Correct it 10 days later: the 2026-01-06 version is now superseded
        // and older than 7d.
        let out = append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &corrected,
                covered: cov(),
                received_at: ts("2026-01-16T09:00:00Z"),
            },
        )
        .unwrap();
        assert_eq!(out.appended, 1);
        assert_eq!(out.swept, 1, "the superseded 2026 version by retention");
        let got = all_rows(&store);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].5, 15.5);
        assert_eq!(
            coverage_count(&store),
            1,
            "the 2026 coverage stays, its two equal spans coalesced"
        );
    }

    #[test]
    fn retention_keeps_a_superseded_version_inside_its_window() {
        let (_d, store, ds) = fixture(); // retention 7d
        let rows = surface_rows("2026-01-05", 1, 15.0);
        let mut corrected = rows.clone();
        corrected.values[0][0] = 15.5;
        let cov = || spx("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z");
        for (r, at) in [
            (&rows, "2026-01-06T09:00:00Z"),
            (&corrected, "2026-01-08T09:00:00Z"),
        ] {
            append_history(
                &store,
                &HistoryAppendRequest {
                    dataset: &ds,
                    source: "hanweck",
                    rows: r,
                    covered: cov(),
                    received_at: ts(at),
                },
            )
            .unwrap();
        }
        assert_eq!(all_rows(&store).len(), 2, "two days old, inside 7d");
    }

    #[test]
    fn the_live_version_survives_retention_whatever_its_age() {
        let (_d, store, ds) = fixture(); // retention 7d
        let rows = surface_rows("2026-01-05", 1, 15.0);
        let mut other = rows.clone();
        other.keys[2] = vec!["0.25".into()];
        let cov = || spx("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z");
        for (r, at) in [
            (&rows, "2026-01-06T09:00:00Z"),
            (&other, "2026-01-30T09:00:00Z"),
        ] {
            append_history(
                &store,
                &HistoryAppendRequest {
                    dataset: &ds,
                    source: "hanweck",
                    rows: r,
                    covered: cov(),
                    received_at: ts(at),
                },
            )
            .unwrap();
        }
        let got = all_rows(&store);
        assert_eq!(got.len(), 2, "a lone version is live, never superseded");
        assert_eq!(got[0].1, micros(ts("2026-01-06T09:00:00Z")));
    }

    #[test]
    fn a_history_dataset_has_no_live_archive_pair_and_two_managed_tables() {
        let ds = history_dataset();
        assert!(crate::store::ddl::table_pairs(&ds).is_empty());
        assert!(crate::store::ddl::history_of("vol_surface", &ds).is_empty());
        let names: Vec<String> = crate::store::managed_tables(&ds)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(
            names,
            ["vol_surface_history", "vol_surface_history_coverage"]
        );
    }

    #[test]
    fn a_fresh_history_dataset_does_not_drift() {
        let (_d, store, ds) = fixture();
        assert!(store.check_drift(&ds).unwrap().is_none());
    }

    /// Record coverage for `asked` over `span` with no rows.
    fn cover(store: &Store, ds: &DatasetSpec, asked: &[&str], span: (&str, &str), at: &str) {
        let rows = HistoryRows {
            ts: vec![],
            keys: vec![vec![]; 3],
            values: vec![vec![]; 2],
        };
        append_history(
            store,
            &HistoryAppendRequest {
                dataset: ds,
                source: "hanweck",
                rows: &rows,
                covered: vec![Covered {
                    asked: asked.iter().map(|s| s.to_string()).collect(),
                    span: (ts(span.0), ts(span.1)),
                }],
                received_at: ts(at),
            },
        )
        .unwrap();
    }

    /// Store one observation of the exact `key` at `at_ts`, recording no
    /// coverage.
    fn observe(store: &Store, ds: &DatasetSpec, key: [&str; 3], at_ts: DateTime<Utc>) {
        let rows = HistoryRows {
            ts: vec![at_ts],
            keys: key.iter().map(|k| vec![k.to_string()]).collect(),
            values: vec![vec![15.0], vec![7650.0]],
        };
        append_history(
            store,
            &HistoryAppendRequest {
                dataset: ds,
                source: "hanweck",
                rows: &rows,
                covered: vec![],
                received_at: ts("2026-06-01T00:00:00Z"),
            },
        )
        .unwrap();
    }

    fn exact(u: &str) -> Vec<String> {
        vec![u.to_string(), "3M".to_string(), "0.5".to_string()]
    }

    #[test]
    fn a_prefix_row_covers_every_deeper_tuple_but_a_deeper_row_never_covers_a_prefix() {
        let (_d, store, ds) = fixture();
        cover(
            &store,
            &ds,
            &["SPX"],
            ("2026-01-05T00:00:00Z", "2026-01-10T00:00:00Z"),
            "2026-01-10T09:00:00Z",
        );
        cover(
            &store,
            &ds,
            &["NDX", "3M", "0.5"],
            ("2026-01-05T00:00:00Z", "2026-01-10T00:00:00Z"),
            "2026-01-10T09:00:00Z",
        );
        let conn = store.writer();
        assert_eq!(
            covered_spans(conn, &ds, &exact("SPX")).unwrap(),
            vec![(ts("2026-01-05T00:00:00Z"), ts("2026-01-10T00:00:00Z"))],
            "the SPX prefix covers its tuples"
        );
        assert_eq!(
            covered_spans(conn, &ds, &["SPX".to_string()])
                .unwrap()
                .len(),
            1
        );
        assert!(
            covered_spans(conn, &ds, &["NDX".to_string()])
                .unwrap()
                .is_empty(),
            "an exact NDX tuple does not cover the NDX prefix"
        );
        assert_eq!(covered_spans(conn, &ds, &exact("NDX")).unwrap().len(), 1);
        assert!(
            covered_spans(conn, &ds, &["SX5E".to_string()])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn an_f64_key_asked_in_another_spelling_finds_its_coverage() {
        let (_d, store, ds) = fixture();
        cover(
            &store,
            &ds,
            &["SPX", "3M", "0.50"],
            ("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"),
            "2026-01-06T09:00:00Z",
        );
        assert_eq!(
            covered_spans(store.writer(), &ds, &exact("SPX"))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn missing_by_key_subtracts_each_tuple_s_own_coverage() {
        let (_d, store, ds) = fixture();
        cover(
            &store,
            &ds,
            &["SPX"],
            ("2026-01-05T00:00:00Z", "2026-01-07T00:00:00Z"),
            "2026-01-07T09:00:00Z",
        );
        let asked = vec![vec!["SPX".to_string()], vec!["SX5E".to_string()]];
        let gaps = missing_by_key(
            store.writer(),
            &ds,
            &asked,
            (ts("2026-01-05T00:00:00Z"), ts("2026-01-10T00:00:00Z")),
        )
        .unwrap();
        assert_eq!(
            gaps[0],
            (
                vec!["SPX".to_string()],
                vec![(ts("2026-01-07T00:00:00Z"), ts("2026-01-10T00:00:00Z"))]
            )
        );
        assert_eq!(
            gaps[1],
            (
                vec!["SX5E".to_string()],
                vec![(ts("2026-01-05T00:00:00Z"), ts("2026-01-10T00:00:00Z"))]
            )
        );
    }

    /// The answer the per-tuple read gives: `covered_spans` for each tuple,
    /// subtracted from `requested`.
    fn per_tuple(
        store: &Store,
        ds: &DatasetSpec,
        asked: &[Vec<String>],
        requested: CoverSpan,
    ) -> Vec<KeyGaps> {
        asked
            .iter()
            .map(|tuple| {
                let held = covered_spans(store.writer(), ds, tuple).unwrap();
                (tuple.clone(), missing_spans(requested, &held))
            })
            .collect()
    }

    /// The batched read answers exactly what the per-tuple read did: a prefix
    /// coverage row applies to every tuple it prefixes, an exact row only to
    /// its tuple, and a tuple with no rows lacks the whole span.
    #[test]
    fn missing_by_key_batches_without_changing_its_answer() {
        let (_d, store, ds) = fixture();
        let (t0, t1, t2) = (
            "2026-01-01T00:00:00Z",
            "2026-01-11T00:00:00Z",
            "2026-01-21T00:00:00Z",
        );
        cover(&store, &ds, &["SPX"], (t0, t1), "2026-01-21T09:00:00Z");
        cover(
            &store,
            &ds,
            &["SPX", "3M", "0.5"],
            (t1, t2),
            "2026-01-21T09:00:00Z",
        );
        let asked = vec![
            exact("SPX"),
            vec!["SPX".to_string(), "1M".to_string(), "0.50".to_string()],
            exact("NDX"),
        ];
        let requested = (ts(t0), ts(t2));
        let got = missing_by_key(store.writer(), &ds, &asked, requested).unwrap();
        assert_eq!(got, per_tuple(&store, &ds, &asked, requested));
        assert_eq!(got[0].0, exact("SPX"), "tuples come back as asked");
        assert_eq!(
            got[1].0[2], "0.50",
            "an f64 key keeps the asked spelling in the answer"
        );
        assert!(
            got[0].1.is_empty(),
            "3M: prefix + exact cover the whole span"
        );
        assert_eq!(
            got[1].1,
            vec![(ts(t1), ts(t2))],
            "1M: only the prefix applies"
        );
        assert_eq!(got[2].1, vec![(ts(t0), ts(t2))], "NDX: nothing applies");
    }

    /// A tuple shorter than the key is covered only by rows at its own depth
    /// or shallower, as `covered_spans` reads it: the exact row under it
    /// does not cover the prefix.
    #[test]
    fn missing_by_key_batches_a_prefix_ask_like_covered_spans() {
        let (_d, store, ds) = fixture();
        let (t0, t1, t2) = (
            "2026-01-01T00:00:00Z",
            "2026-01-11T00:00:00Z",
            "2026-01-21T00:00:00Z",
        );
        cover(&store, &ds, &["SPX"], (t0, t1), "2026-01-21T09:00:00Z");
        cover(
            &store,
            &ds,
            &["SPX", "3M"],
            (t1, t2),
            "2026-01-21T09:00:00Z",
        );
        cover(
            &store,
            &ds,
            &["SPX", "1M", "0.5"],
            (t0, t2),
            "2026-01-21T09:00:00Z",
        );
        let asked = vec![
            vec!["SPX".to_string(), "3M".to_string()],
            vec!["SPX".to_string(), "1M".to_string()],
            vec!["SPX".to_string()],
            exact("SPX"),
            // Brings the SPX/1M/0.5 row into the batch's result, so the
            // per-tuple pick, not the statement, must keep it off SPX/1M.
            vec!["SPX".to_string(), "1M".to_string(), "0.5".to_string()],
        ];
        let requested = (ts(t0), ts(t2));
        let got = missing_by_key(store.writer(), &ds, &asked, requested).unwrap();
        assert_eq!(got, per_tuple(&store, &ds, &asked, requested));
        assert!(
            got[0].1.is_empty(),
            "SPX/3M: its own row and the SPX prefix"
        );
        assert_eq!(
            got[1].1,
            vec![(ts(t1), ts(t2))],
            "SPX/1M: the exact row under it does not cover it"
        );
        assert_eq!(got[2].1, vec![(ts(t1), ts(t2))], "SPX: only its own row");
        assert!(got[3].1.is_empty(), "SPX/3M/0.5: both shallower rows");
        assert!(got[4].1.is_empty(), "SPX/1M/0.5: its own row");
    }

    /// The batched read is one statement whatever the tuple count: one
    /// select over a VALUES list with a row per asked tuple, each row a
    /// text cell per key column.
    #[test]
    fn coverage_batch_sql_is_one_select_with_a_values_row_per_tuple() {
        let ds = history_dataset();
        let keys = ds.history_key();
        let sql = coverage_batch_sql(&ds.name, &keys, 250);
        assert_eq!(sql.to_lowercase().matches("select").count(), 1, "{sql}");
        assert_eq!(
            sql.matches("(CAST(? AS VARCHAR)").count(),
            250,
            "one VALUES row per tuple"
        );
        assert_eq!(sql.matches("CAST(? AS VARCHAR)").count(), 250 * keys.len());
        assert_eq!(sql.matches('?').count(), 250 * keys.len());
    }

    #[test]
    fn missing_by_key_refuses_a_tuple_longer_than_the_key_before_any_sql() {
        let (_d, store, ds) = fixture();
        let asked = vec![
            exact("SPX"),
            vec!["SPX".into(), "3M".into(), "0.5".into(), "x".into()],
        ];
        let requested = (ts("2026-01-01T00:00:00Z"), ts("2026-01-02T00:00:00Z"));
        assert!(matches!(
            missing_by_key(store.writer(), &ds, &asked, requested),
            Err(StoreError::History(_))
        ));
        assert!(
            missing_by_key(store.writer(), &ds, &[], requested)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn covers_instant_needs_every_tuple_and_a_point_span_counts() {
        let (_d, store, ds) = fixture();
        cover(
            &store,
            &ds,
            &["SPX"],
            ("2026-01-05T00:00:00Z", "2026-01-07T00:00:00Z"),
            "2026-01-07T09:00:00Z",
        );
        observe(
            &store,
            &ds,
            ["SPX", "3M", "0.5"],
            ts("2026-01-05T21:00:00Z"),
        );
        let at = ts("2026-01-06T21:00:00Z");
        let point = (at, at + chrono::Duration::microseconds(1));
        cover(
            &store,
            &ds,
            &["SX5E", "3M", "0.5"],
            (&point.0.to_rfc3339(), &point.1.to_rfc3339()),
            "2026-01-07T09:00:00Z",
        );
        observe(&store, &ds, ["SX5E", "3M", "0.5"], at);
        let both = vec![exact("SPX"), exact("SX5E")];
        assert!(covers_instant(store.writer(), &ds, &both, at).unwrap());
        assert!(!covers_instant(store.writer(), &ds, &both, ts("2026-01-08T21:00:00Z")).unwrap());
        assert!(!covers_instant(store.writer(), &ds, &[exact("NDX")], at).unwrap());
        assert!(
            !covers_instant(store.writer(), &ds, &[exact("SPX"), exact("NDX")], at).unwrap(),
            "one held tuple does not answer for an unheld one beside it"
        );
    }

    #[test]
    fn covers_instant_is_false_when_the_span_holds_t_but_no_row_at_or_before_it() {
        let (_d, store, ds) = fixture();
        cover(
            &store,
            &ds,
            &["SPX", "3M", "0.5"],
            ("2026-01-05T00:00:00Z", "2026-01-10T00:00:00Z"),
            "2026-01-10T09:00:00Z",
        );
        // A row after t, and one before the span: neither is an observation
        // the cache really holds as the last at or before t.
        observe(
            &store,
            &ds,
            ["SPX", "3M", "0.5"],
            ts("2026-01-08T21:00:00Z"),
        );
        observe(
            &store,
            &ds,
            ["SPX", "3M", "0.5"],
            ts("2026-01-02T21:00:00Z"),
        );
        // Another tuple's row inside [a, t] does not count either.
        observe(
            &store,
            &ds,
            ["SPX", "3M", "0.25"],
            ts("2026-01-05T21:00:00Z"),
        );
        assert!(
            !covers_instant(
                store.writer(),
                &ds,
                &[exact("SPX")],
                ts("2026-01-06T12:00:00Z")
            )
            .unwrap()
        );
    }

    #[test]
    fn covers_instant_is_true_with_a_row_between_the_span_start_and_t() {
        let (_d, store, ds) = fixture();
        cover(
            &store,
            &ds,
            &["SPX", "3M", "0.5"],
            ("2026-01-05T00:00:00Z", "2026-01-10T00:00:00Z"),
            "2026-01-10T09:00:00Z",
        );
        observe(
            &store,
            &ds,
            ["SPX", "3M", "0.5"],
            ts("2026-01-05T00:00:00Z"),
        );
        let w = store.writer();
        assert!(
            covers_instant(w, &ds, &[exact("SPX")], ts("2026-01-06T12:00:00Z")).unwrap(),
            "a row at the span start"
        );
        assert!(
            covers_instant(w, &ds, &[exact("SPX")], ts("2026-01-05T00:00:00Z")).unwrap(),
            "a row exactly at t"
        );
        // The f64 key asked in another spelling is the same tuple.
        let spelled = vec!["SPX".to_string(), "3M".to_string(), "0.50".to_string()];
        assert!(covers_instant(w, &ds, &[spelled], ts("2026-01-06T12:00:00Z")).unwrap());
    }

    #[test]
    fn covers_instant_is_false_for_a_prefix_ask_whatever_its_coverage() {
        let (_d, store, ds) = fixture();
        cover(
            &store,
            &ds,
            &["SPX"],
            ("2026-01-05T00:00:00Z", "2026-01-10T00:00:00Z"),
            "2026-01-10T09:00:00Z",
        );
        observe(
            &store,
            &ds,
            ["SPX", "3M", "0.5"],
            ts("2026-01-05T21:00:00Z"),
        );
        let t = ts("2026-01-06T12:00:00Z");
        let w = store.writer();
        assert!(!covers_instant(w, &ds, &[vec!["SPX".to_string()]], t).unwrap());
        assert!(!covers_instant(w, &ds, &[vec!["SPX".to_string(), "3M".to_string()]], t).unwrap());
        assert!(
            covers_instant(w, &ds, &[exact("SPX")], t).unwrap(),
            "the exact tuple under it is held"
        );
    }

    #[test]
    fn uncovered_at_names_the_tuples_the_cache_cannot_answer() {
        let (_d, store, ds) = seeded();
        // SPX covered over its rows; NDX covered from the 6th, after its only row.
        let week = ("2026-01-05T00:00:00Z", "2026-01-10T00:00:00Z");
        cover(
            &store,
            &ds,
            &["SPX", "3M", "0.5"],
            week,
            "2026-01-10T00:00:00Z",
        );
        cover(
            &store,
            &ds,
            &["NDX", "3M", "0.5"],
            ("2026-01-06T00:00:00Z", "2026-01-10T00:00:00Z"),
            "2026-01-10T00:00:00Z",
        );
        let conn = store.writer();
        let spx = exact("SPX");
        let ndx = exact("NDX");
        let at = ts("2026-01-08T12:00:00Z");
        assert_eq!(
            uncovered_at(conn, &ds, std::slice::from_ref(&spx), at).unwrap(),
            Vec::<Vec<String>>::new(),
            "SPX has a row at or before t inside its span"
        );
        assert_eq!(
            uncovered_at(conn, &ds, std::slice::from_ref(&ndx), at).unwrap(),
            vec![ndx.clone()],
            "NDX is covered but its only row lies before the span"
        );
        assert_eq!(
            uncovered_at(conn, &ds, &[ndx.clone(), spx.clone()], at).unwrap(),
            vec![ndx.clone()],
            "only the tuple that cannot be answered"
        );
        let sx5e = exact("SX5E");
        assert_eq!(
            uncovered_at(
                conn,
                &ds,
                &[sx5e.clone(), spx.clone(), ndx.clone(), sx5e.clone()],
                at
            )
            .unwrap(),
            vec![sx5e.clone(), ndx.clone()],
            "asked order, an uncovered tuple once however often asked"
        );
        assert!(
            !covers_instant(conn, &ds, &[spx.clone(), ndx], at).unwrap(),
            "every tuple, not any"
        );
        assert!(
            covers_instant(conn, &ds, &[spx.clone(), spx], at).unwrap(),
            "a repeated tuple is one tuple"
        );
    }

    #[test]
    fn covers_instant_reads_a_prefix_coverage_row_for_an_exact_tuple() {
        let (_d, store, ds) = seeded();
        cover(
            &store,
            &ds,
            &["SPX"],
            ("2026-01-05T00:00:00Z", "2026-01-10T00:00:00Z"),
            "2026-01-10T00:00:00Z",
        );
        let conn = store.writer();
        let spx = exact("SPX");
        let spx25 = vec!["SPX".to_string(), "3M".into(), "0.25".into()];
        let at = ts("2026-01-08T12:00:00Z");
        assert!(covers_instant(conn, &ds, &[spx, spx25], at).unwrap());
        assert_eq!(
            uncovered_at(conn, &ds, &[exact("NDX")], at).unwrap(),
            vec![exact("NDX")],
            "another member's prefix row does not cover it"
        );
    }

    #[test]
    fn a_tuple_of_the_wrong_arity_makes_every_asked_tuple_uncovered() {
        let (_d, store, ds) = seeded();
        cover(
            &store,
            &ds,
            &["SPX"],
            ("2026-01-05T00:00:00Z", "2026-01-10T00:00:00Z"),
            "2026-01-10T00:00:00Z",
        );
        let conn = store.writer();
        let at = ts("2026-01-08T12:00:00Z");
        let prefix = vec!["SPX".to_string()];
        let long = vec!["SPX".to_string(), "3M".into(), "0.5".into(), "x".into()];
        assert!(
            !covers_instant(conn, &ds, std::slice::from_ref(&prefix), at).unwrap(),
            "a one-column prefix is Ok(false), never an error"
        );
        assert!(!covers_instant(conn, &ds, std::slice::from_ref(&long), at).unwrap());
        assert_eq!(
            uncovered_at(conn, &ds, &[exact("SPX"), prefix.clone(), exact("SPX")], at).unwrap(),
            vec![exact("SPX"), prefix],
            "every asked tuple, as asked, repeats removed"
        );
    }

    /// SPX 3M at deltas 0.5 and 0.25, three days each; NDX 3M 0.5 one day.
    fn seeded() -> (tempfile::TempDir, Store, DatasetSpec) {
        let (d, store, ds) = fixture();
        let mut rows = HistoryRows::default();
        for (u, delta, days, base) in [
            ("SPX", "0.5", 3, 15.0),
            ("SPX", "0.25", 3, 18.0),
            ("NDX", "0.5", 1, 19.0),
        ] {
            for i in 0..days {
                rows.ts.push(ts(&format!("2026-01-0{}T21:00:00Z", 5 + i)));
                rows.values.resize(2, Vec::new());
                rows.values[0].push(base + i as f64);
                rows.values[1].push(7650.0);
                rows.keys.resize(3, Vec::new());
                rows.keys[0].push(u.into());
                rows.keys[1].push("3M".into());
                rows.keys[2].push(delta.into());
            }
        }
        append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &rows,
                covered: vec![],
                received_at: ts("2026-01-08T09:00:00Z"),
            },
        )
        .unwrap();
        // A correction of SPX 0.5 on the 6th, received later: live must read 99.
        let mut fix = HistoryRows::default();
        fix.ts.push(ts("2026-01-06T21:00:00Z"));
        fix.keys = vec![vec!["SPX".into()], vec!["3M".into()], vec!["0.5".into()]];
        fix.values = vec![vec![99.0], vec![7650.0]];
        append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &fix,
                covered: vec![],
                received_at: ts("2026-01-09T09:00:00Z"),
            },
        )
        .unwrap();
        (d, store, ds)
    }

    #[test]
    fn latest_answers_the_greatest_ts_per_key_under_a_prefix() {
        let (_d, store, ds) = seeded();
        let t = read_history(
            store.writer(),
            &ds,
            &KeySet::Prefix(vec![vec!["SPX".into()]]),
            Span::Latest,
        )
        .unwrap();
        assert_eq!(t.key_columns, ["underlying_ref", "tenor", "delta"]);
        assert_eq!(t.value_columns, ["vol", "forward"]);
        assert_eq!(t.rows.len(), 2, "one per SPX key");
        assert_eq!(t.rows[0].key, ["SPX", "3M", "0.25"]);
        assert_eq!(t.rows[1].key, ["SPX", "3M", "0.5"]);
        assert_eq!(t.rows[1].ts, ts("2026-01-07T21:00:00Z"));
        assert_eq!(t.rows[1].values, [17.0, 7650.0]);
    }

    #[test]
    fn at_answers_the_last_observation_at_or_before_t_with_the_live_version() {
        let (_d, store, ds) = seeded();
        let exact = KeySet::Exact(vec![vec!["SPX".into(), "3M".into(), "0.50".into()]]);
        let t = read_history(
            store.writer(),
            &ds,
            &exact,
            Span::At(ts("2026-01-06T21:00:00Z")),
        )
        .unwrap();
        assert_eq!(t.rows.len(), 1);
        assert_eq!(
            t.rows[0].ts,
            ts("2026-01-06T21:00:00Z"),
            "at or before: the 6th's own point"
        );
        assert_eq!(t.rows[0].values[0], 99.0, "the correction is live");
        assert_eq!(
            t.rows[0].key[2], "0.5",
            "asked as 0.50, answered as the one key"
        );
        let before = read_history(
            store.writer(),
            &ds,
            &exact,
            Span::At(ts("2026-01-05T00:00:00Z")),
        )
        .unwrap();
        assert!(
            before.rows.is_empty(),
            "nothing at or before the first point"
        );
    }

    #[test]
    fn between_is_half_open_and_ordered_by_key_then_ts() {
        let (_d, store, ds) = seeded();
        let t = read_history(
            store.writer(),
            &ds,
            &KeySet::Prefix(vec![vec!["SPX".into()], vec!["NDX".into()]]),
            Span::Between {
                from: ts("2026-01-06T21:00:00Z"),
                to: ts("2026-01-07T21:00:00Z"),
                step: None,
            },
        )
        .unwrap();
        let got: Vec<(Vec<String>, DateTime<Utc>)> =
            t.rows.iter().map(|r| (r.key.clone(), r.ts)).collect();
        assert_eq!(
            got,
            vec![
                (
                    vec!["SPX".into(), "3M".into(), "0.25".into()],
                    ts("2026-01-06T21:00:00Z")
                ),
                (
                    vec!["SPX".into(), "3M".into(), "0.5".into()],
                    ts("2026-01-06T21:00:00Z")
                ),
            ],
            "the 7th is excluded, NDX has no point in range"
        );
    }

    #[test]
    fn a_key_set_of_the_wrong_arity_is_refused_before_sql() {
        let (_d, store, ds) = seeded();
        let err = read_history(
            store.writer(),
            &ds,
            &KeySet::Exact(vec![vec!["SPX".into()]]),
            Span::Latest,
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::History(_)));
    }

    proptest! {
        // Each case opens its own store and runs up to a dozen append
        // transactions: 256 cases cost close to a minute alone and far more
        // beside the rest of the suite on a loaded machine.
        #![proptest_config(ProptestConfig::with_cases(48))]

        /// Coverage written through any sequence of prefix and exact spans,
        /// with observations on any days, never reports a tuple answerable
        /// at an instant unless its coverage holds every moment from a
        /// stored observation of it up to that instant; a prefix never is.
        #[test]
        fn coverage_never_over_reports(
            spans in proptest::collection::vec((0u32..40, 1u32..10, 0u8..2), 0..8),
            rows in proptest::collection::vec(0u32..50, 0..4),
            probe in 0u32..50,
        ) {
            let (_d, store, ds) = fixture();
            let base = ts("2026-01-01T00:00:00Z");
            let day = |d: u32| base + chrono::Duration::days(d as i64);
            let mut truth: Vec<(bool, u32, u32)> = Vec::new(); // (is_prefix, first day, end day)
            for (start, len, kind) in spans {
                let asked: Vec<&str> = if kind == 0 { vec!["SPX"] } else { vec!["SPX", "3M", "0.5"] };
                cover(&store, &ds, &asked, (&day(start).to_rfc3339(), &day(start + len).to_rfc3339()), "2026-06-01T00:00:00Z");
                truth.push((kind == 0, start, start + len));
            }
            for &d in &rows {
                observe(&store, &ds, ["SPX", "3M", "0.5"], day(d) + chrono::Duration::hours(6));
            }
            // Spans are whole days and rows and the probe sit inside a day,
            // so [row, t] is covered exactly when every day it touches is.
            let day_covered = |d: u32| truth.iter().any(|&(_, f, to)| f <= d && d < to);
            let expect_exact = rows.iter().any(|&r| r <= probe && (r..=probe).all(day_covered));
            let t = day(probe) + chrono::Duration::hours(12);
            let w = store.writer();
            prop_assert_eq!(covers_instant(w, &ds, &[exact("SPX")], t).unwrap(), expect_exact);
            let prefix = vec![vec!["SPX".to_string()]];
            prop_assert!(!covers_instant(w, &ds, &prefix, t).unwrap(), "a prefix is never answered as current");
            // The prefix's own coverage sees prefix rows only.
            let expect_prefix = truth.iter().any(|&(p, f, to)| p && f <= probe && probe < to);
            let held = covered_spans(w, &ds, &["SPX".to_string()]).unwrap();
            prop_assert_eq!(held.iter().any(|(f, to)| *f <= t && t < *to), expect_prefix);
        }
    }
}
