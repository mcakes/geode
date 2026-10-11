//! The board read: for each dataset a board names, one SQL statement
//! projects the cached rows of the asked tuples to observations (the
//! latest, the last at or before each lookback instant, the last of each
//! UTC day inside the daily span, the earliest cached), and
//! `geode_core::volboard::reduce` turns them into rows. Runs on a pool
//! worker after the service fetched what coverage lacked.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use duckdb::Connection;
use duckdb::types::Value;
use geode_core::query::{QueryKey, normalize_key};
use geode_core::schema::{ColumnSpec, ColumnType, DatasetSpec, SchemaSpec};
use geode_core::volboard::plan::{daily_span, day_index};
use geode_core::volboard::reduce::{Observations, Observed, reduce};
use geode_core::volboard::{Input, Role, VolBoard, VolBoardParams};

use crate::store::StoreError;
use crate::store::history::history_table;
use crate::store::series::{from_micros, micros};

/// What the service fixed at submission: the request, `now`, the lookback
/// instants (parallel to `params.lookbacks`) and the datasets whose calls
/// failed (their cells blank, the board otherwise current).
#[derive(Debug, Clone, PartialEq)]
pub struct BoardPlan {
    pub params: VolBoardParams,
    pub now: DateTime<Utc>,
    pub instants: Vec<DateTime<Utc>>,
    pub dataset_errors: Vec<(String, String)>,
}

/// The pool key the board read runs under: the requester's with bit 63
/// flipped. The pool coalesces and cancels per key, and the requester's
/// own key carries its view queries (Part 4's vega); tile ids are small
/// counters and the shell's reserved keys are `u64::MAX - n`, so neither
/// lands here.
pub fn board_pool_key(requester: QueryKey) -> QueryKey {
    QueryKey(requester.0 ^ (1 << 63))
}

// Row kinds the SQL emits, in `order by kind, idx, keys` order.
const KIND_CURRENT: i32 = 0;
const KIND_AT: i32 = 1;
const KIND_DAILY: i32 = 2;
const KIND_FIRST: i32 = 3;
const DAY_US: i64 = 86_400_000_000;

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn joined(cols: &[&ColumnSpec], f: impl Fn(&ColumnSpec) -> String, sep: &str) -> String {
    cols.iter().map(|c| f(c)).collect::<Vec<_>>().join(sep)
}

/// The key arity and value columns a role's dataset must declare.
fn shape(role: Role) -> (usize, &'static [&'static str]) {
    match role {
        Role::Surface => (3, &["vol", "forward"]),
        Role::Realized => (2, &["vol"]),
        Role::Varswap => (2, &["strike"]),
    }
}

/// Refuses a dataset the board cannot read: undeclared or not history,
/// the wrong key arity, or a value column the role needs missing. Shared
/// with the service's submission check so both name the same gap.
pub fn check_dataset<'a>(
    schema: &'a SchemaSpec,
    name: &str,
    role: Role,
) -> Result<&'a DatasetSpec, String> {
    let ds = schema
        .dataset(name)
        .filter(|d| d.is_history())
        .ok_or_else(|| format!("'{name}' is not a declared history dataset"))?;
    let (arity, columns) = shape(role);
    if ds.key.len() != arity {
        return Err(format!(
            "'{name}' has a {}-column key; the board needs {arity}",
            ds.key.len()
        ));
    }
    let values = ds.history_values();
    for column in columns {
        if !values.iter().any(|c| c.name == *column) {
            return Err(format!("'{name}' has no value column '{column}'"));
        }
    }
    Ok(ds)
}

/// One statement: the live version of every asked tuple's rows, then the
/// four projections unioned with a `kind` and an `idx` (the lookback
/// position for `KIND_AT`, the day index for `KIND_DAILY`). `tuples` is
/// not empty (a `values` list with no rows does not parse) and holds each
/// key in its canonical text spelling.
fn board_sql(
    ds: &DatasetSpec,
    tuples: &[Vec<String>],
    instants: &[DateTime<Utc>],
    daily: Option<(DateTime<Utc>, DateTime<Utc>)>,
) -> (String, Vec<Value>) {
    let keys = ds.history_key();
    let values = ds.history_values();
    let table = history_table(&ds.name);
    let key_list = joined(&keys, |c| quote(&c.name), ", ");
    let value_list = joined(&values, |c| quote(&c.name), ", ");
    let live_values = joined(
        &values,
        |c| format!("arg_max({0}, received_at) as {0}", quote(&c.name)),
        ", ",
    );
    let mut params: Vec<Value> = Vec::new();
    // The asked tuples as a VALUES list joined on the key, each cell bound
    // as text and cast to the declared type in the join. These bind first:
    // the `live` CTE is the first parameterised text in the statement.
    let row = format!(
        "({})",
        keys.iter()
            .map(|_| "CAST(? AS VARCHAR)")
            .collect::<Vec<_>>()
            .join(", ")
    );
    let asked_rows = tuples
        .iter()
        .map(|_| row.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    for tuple in tuples {
        params.extend(tuple.iter().map(|v| Value::Text(v.clone())));
    }
    let asked_cols = (0..keys.len())
        .map(|i| format!("a{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let on = keys
        .iter()
        .enumerate()
        .map(|(i, c)| format!("t.{} = CAST(asked.a{i} AS {})", quote(&c.name), c.ty.sql()))
        .collect::<Vec<_>>()
        .join(" and ");
    let live = format!(
        "live as (
             select {key_list}, ts, {live_values} from {table} t
             join (values {asked_rows}) as asked({asked_cols}) on {on}
             group by {key_list}, ts
         )"
    );
    let projection = format!("{key_list}, epoch_us(ts), {value_list}");
    let mut selects = vec![
        format!(
            "select {KIND_CURRENT} as kind, 0::BIGINT as idx, {projection} from (
                 select *, row_number() over (partition by {key_list} order by ts desc) as rn from live
             ) where rn = 1"
        ),
        format!(
            "select {KIND_FIRST} as kind, 0::BIGINT as idx, {projection} from (
                 select *, row_number() over (partition by {key_list} order by ts asc) as rn from live
             ) where rn = 1"
        ),
    ];
    // Then the instants, in select order, then the daily span: DuckDB
    // binds `?` in textual order, and the selects follow the CTE.
    for (i, t) in instants.iter().enumerate() {
        selects.push(format!(
            "select {KIND_AT} as kind, {i}::BIGINT as idx, {projection} from (
                 select *, row_number() over (partition by {key_list} order by ts desc) as rn
                 from live where ts <= make_timestamp(?)
             ) where rn = 1"
        ));
        params.push(Value::BigInt(micros(*t)));
    }
    if let Some((from, to)) = daily {
        // DuckDB's `//` on BIGINT truncates toward zero (-1 // DAY_US is
        // 0), where `plan::day_index` floors: the two agree from 1970 on,
        // and a cached observation before 1970 would bucket one day late.
        selects.push(format!(
            "select {KIND_DAILY} as kind, epoch_us(ts) // {DAY_US} as idx, {projection} from (
                 select *, row_number() over (partition by {key_list}, epoch_us(ts) // {DAY_US} order by ts desc) as rn
                 from live where ts >= make_timestamp(?) and ts < make_timestamp(?)
             ) where rn = 1"
        ));
        params.extend([Value::BigInt(micros(from)), Value::BigInt(micros(to))]);
    }
    let sql = format!(
        "with {live} {} order by kind, idx, {key_list}",
        selects.join(" union all ")
    );
    (sql, params)
}

/// Read one dataset's observations of `inputs` (all of `role`) into
/// `obs`, keyed by each request member and each input exactly as given:
/// `reduce` looks them up by those values. Members and suffixes are
/// compared with the stored keys in `normalize_key`'s spelling, so a
/// suffix spelt `0.250` reads the `0.25` key. A key that does not
/// normalize names no stored row and reads nothing. The observations
/// reach `obs` only once the whole statement has been read: a read failing
/// partway (rows come by kind, so current and lookback rows precede the
/// daily ones) must not leave a current or a truncated day set beside the
/// dataset's error.
fn read_role(
    conn: &Connection,
    schema: &SchemaSpec,
    plan: &BoardPlan,
    role: Role,
    inputs: &[Input],
    obs: &mut Observations,
) -> Result<(), StoreError> {
    let name = plan
        .params
        .dataset(role)
        .expect("validated: a role in use names a dataset");
    let ds = check_dataset(schema, name, role).map_err(StoreError::History)?;
    let keys = ds.history_key();
    let values = ds.history_values();
    let column_index: HashMap<&str, usize> = values
        .iter()
        .enumerate()
        .map(|(i, c)| (c.name.as_str(), i))
        .collect();
    // Canonical member -> the request's spellings of it.
    let mut members: HashMap<String, Vec<&str>> = HashMap::new();
    for m in &plan.params.members {
        if let Ok(canonical) = normalize_key(m, keys[0].ty) {
            let spellings = members.entry(canonical).or_default();
            if !spellings.contains(&m.as_str()) {
                spellings.push(m.as_str());
            }
        }
    }
    // Canonical suffix -> the inputs asking for it, in first-seen order.
    let mut by_suffix: Vec<(Vec<String>, Vec<&Input>)> = Vec::new();
    for input in inputs {
        let canonical: Option<Vec<String>> = (input.suffix.len() + 1 == keys.len())
            .then(|| {
                keys[1..]
                    .iter()
                    .zip(&input.suffix)
                    .map(|(c, v)| normalize_key(v, c.ty).ok())
                    .collect()
            })
            .flatten();
        let Some(canonical) = canonical else { continue };
        match by_suffix.iter_mut().find(|(s, _)| *s == canonical) {
            Some((_, list)) => list.push(input),
            None => by_suffix.push((canonical, vec![input])),
        }
    }
    let mut member_keys: Vec<&String> = members.keys().collect();
    member_keys.sort();
    let tuples: Vec<Vec<String>> = member_keys
        .iter()
        .flat_map(|m| {
            by_suffix.iter().map(move |(s, _)| {
                std::iter::once((*m).clone())
                    .chain(s.iter().cloned())
                    .collect()
            })
        })
        .collect();
    if tuples.is_empty() {
        return Ok(());
    }
    let daily = daily_span(&plan.params).and_then(|span| {
        let from = plan
            .now
            .checked_sub_signed(chrono::Duration::from_std(span).ok()?)?;
        Some((from, plan.now))
    });
    let (sql, params) = board_sql(ds, &tuples, &plan.instants, daily);
    let n_keys = keys.len();
    let n_lookbacks = plan.instants.len();
    let err = |e: duckdb::Error| StoreError::Sql {
        statement: sql.clone(),
        source: e,
    };
    let mut found: HashMap<(String, Input), Observed> = HashMap::new();
    let mut stmt = conn.prepare(&sql).map_err(err)?;
    let mut rows = stmt.query(duckdb::params_from_iter(params)).map_err(err)?;
    while let Some(r) = rows.next().map_err(err)? {
        let kind: i32 = r.get(0).map_err(err)?;
        let idx: i64 = r.get(1).map_err(err)?;
        let key: Vec<String> = keys
            .iter()
            .enumerate()
            .map(|(i, c)| match c.ty {
                // Read back in the spelling `normalize_key` writes.
                ColumnType::F64 => r.get::<_, f64>(2 + i).map(|f| format!("{f}")),
                _ => r.get::<_, String>(2 + i),
            })
            .collect::<Result<_, _>>()
            .map_err(err)?;
        let at = from_micros(r.get::<_, i64>(2 + n_keys).map_err(err)?);
        let Some(spellings) = members.get(&key[0]) else {
            continue;
        };
        let Some((_, asking)) = by_suffix.iter().find(|(s, _)| s[..] == key[1..]) else {
            continue;
        };
        for input in asking {
            let Some(&ci) = column_index.get(input.column) else {
                continue;
            };
            let value: f64 = r.get(3 + n_keys + ci).map_err(err)?;
            for member in spellings {
                let o = found
                    .entry((member.to_string(), (*input).clone()))
                    .or_insert_with(|| Observed {
                        at: vec![None; n_lookbacks],
                        ..Default::default()
                    });
                match kind {
                    KIND_CURRENT => o.current = Some((at, value)),
                    KIND_FIRST => o.first = Some(at),
                    KIND_AT => {
                        if let Some(slot) = o.at.get_mut(idx as usize) {
                            *slot = Some((at, value));
                        }
                    }
                    // The projection answers one row per tuple per UTC
                    // day; a second would double-count the day in a rank
                    // or keep whichever mark came last in row order.
                    KIND_DAILY => {
                        let earlier = o.daily.insert(idx, value);
                        if earlier.is_some() {
                            return Err(StoreError::History(format!(
                                "'{name}': two daily observations of one day for {}",
                                key.join(" / ")
                            )));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    obs.by_input.extend(found);
    Ok(())
}

/// Read every dataset the plan names (skipping `dataset_errors`), project
/// each to observations and reduce. A surface read error is the board's
/// `Err`; another dataset's read error joins `dataset_errors`.
pub fn read_board(
    conn: &Connection,
    schema: &SchemaSpec,
    plan: &BoardPlan,
) -> Result<VolBoard, StoreError> {
    let params = &plan.params;
    let mut board = VolBoard {
        dataset_errors: plan.dataset_errors.clone(),
        ..Default::default()
    };
    let days_back = |d: std::time::Duration| {
        Some(day_index(
            plan.now
                .checked_sub_signed(chrono::Duration::from_std(d).ok()?)?,
        ))
    };
    let mut obs = Observations {
        window_from_day: params.window.and_then(days_back),
        spark_from_day: params.sparkline.as_ref().and_then(|(_, s)| days_back(*s)),
        instants: plan.instants.clone(),
        ..Default::default()
    };
    let mut by_role: BTreeMap<Role, Vec<Input>> = BTreeMap::new();
    for input in geode_core::volboard::plan::inputs(params) {
        by_role.entry(input.role).or_default().push(input);
    }
    for (role, inputs) in by_role {
        let Some(name) = params.dataset(role) else {
            continue;
        };
        if board.dataset_errors.iter().any(|(d, _)| d == name) {
            continue;
        }
        match read_role(conn, schema, plan, role, &inputs, &mut obs) {
            Ok(()) => {}
            Err(e) if role == Role::Surface => return Err(e),
            // `StoreError::History` displays as `history: <message>`; the
            // dataset error carries the message alone.
            Err(StoreError::History(m)) => board.dataset_errors.push((name.to_string(), m)),
            Err(e) => board.dataset_errors.push((name.to_string(), e.to_string())),
        }
    }
    let reduced = reduce(params, &obs);
    board.rows = reduced.rows;
    board.missing = reduced.missing;
    board.notes = reduced.notes;
    Ok(board)
}

/// Pool work: `read_board` under the plan.
#[derive(Debug)]
pub struct BoardRead {
    schema: Arc<SchemaSpec>,
    plan: BoardPlan,
}

impl BoardRead {
    pub fn new(schema: Arc<SchemaSpec>, plan: BoardPlan) -> Self {
        Self { schema, plan }
    }

    pub fn plan(&self) -> &BoardPlan {
        &self.plan
    }

    pub(crate) fn run(&self, conn: &Connection) -> Result<VolBoard, StoreError> {
        read_board(conn, &self.schema, &self.plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::HistoryRows;
    use crate::store::ddl::tests_support::{history_dataset, ts};
    use crate::store::history::{HistoryAppendRequest, append_history, history_table};
    use crate::store::{Catalog, Store};
    use geode_core::query::QueryKey;
    use geode_core::schema::SchemaSpec;
    use geode_core::volboard::{Lookback, Measure, VolBoardParams};
    use std::time::{Duration, Instant};

    /// Surface rows: SPX 3M at three deltas over Jan 8–10 with an intraday
    /// mark on the 10th and a superseded version of the 9th; SPX 1M 0.5 on
    /// 2025-01-05 only (outside a 365-day window from 2026-01-11, inside the
    /// 400-day retention from the append); nothing for NDX. Deltas are call
    /// deltas: 0.75 is the 25Δ put, so skew (0.75 − 0.25) is +0.05 now and
    /// +0.04 on the 9th.
    fn seeded() -> (tempfile::TempDir, Store, SchemaSpec) {
        let dir = tempfile::tempdir().unwrap();
        let ds = history_dataset();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        store.apply_schema(&ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        let append = |rows: &[(&str, &str, &str, &str, f64)], received: &str| {
            let mut r = HistoryRows::default();
            r.keys.resize(3, Vec::new());
            r.values.resize(2, Vec::new());
            for (u, tenor, delta, at, vol) in rows {
                r.keys[0].push((*u).into());
                r.keys[1].push((*tenor).into());
                r.keys[2].push((*delta).into());
                r.ts.push(ts(at));
                r.values[0].push(*vol);
                r.values[1].push(4000.0);
            }
            append_history(
                &store,
                &HistoryAppendRequest {
                    dataset: &ds,
                    source: "t",
                    rows: &r,
                    covered: vec![],
                    received_at: ts(received),
                },
            )
            .unwrap();
        };
        // The wrong 9th first, then the real one as a later version.
        append(
            &[("SPX", "3M", "0.5", "2026-01-09T21:00:00Z", 0.99)],
            "2026-01-09T21:05:00Z",
        );
        append(
            &[
                ("SPX", "3M", "0.5", "2026-01-08T21:00:00Z", 0.18),
                ("SPX", "3M", "0.5", "2026-01-09T21:00:00Z", 0.19),
                ("SPX", "3M", "0.5", "2026-01-10T15:00:00Z", 0.195),
                ("SPX", "3M", "0.5", "2026-01-10T21:00:00Z", 0.20),
                ("SPX", "3M", "0.25", "2026-01-08T21:00:00Z", 0.20),
                ("SPX", "3M", "0.25", "2026-01-09T21:00:00Z", 0.20),
                ("SPX", "3M", "0.25", "2026-01-10T21:00:00Z", 0.20),
                ("SPX", "3M", "0.75", "2026-01-08T21:00:00Z", 0.24),
                ("SPX", "3M", "0.75", "2026-01-09T21:00:00Z", 0.24),
                ("SPX", "3M", "0.75", "2026-01-10T21:00:00Z", 0.25),
                ("SPX", "1M", "0.5", "2025-01-05T21:00:00Z", 0.17),
            ],
            "2026-01-10T21:05:00Z",
        );
        let schema = SchemaSpec { datasets: vec![ds] };
        (dir, store, schema)
    }

    /// The seed is only a test of the live version and of a stale current
    /// if the append's sweep kept both rows: count them directly.
    fn assert_the_seed_kept_the_superseded_and_the_old_rows(store: &Store) {
        let table = history_table("vol_surface");
        let count = |filter: &str| -> i64 {
            store
                .writer()
                .query_row(
                    &format!("select count(*) from {table} where {filter}"),
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(
            count(
                "underlying_ref = 'SPX' and tenor = '3M' and delta = 0.5 \
                 and ts = TIMESTAMP '2026-01-09 21:00:00' and vol = 0.99"
            ),
            1,
            "the superseded version of the 9th is still stored"
        );
        assert_eq!(
            count(
                "underlying_ref = 'SPX' and tenor = '3M' and delta = 0.5 \
                 and ts = TIMESTAMP '2026-01-09 21:00:00'"
            ),
            2,
            "both versions of the 9th are stored"
        );
        assert_eq!(
            count("tenor = '1M' and ts = TIMESTAMP '2025-01-05 21:00:00'"),
            1,
            "the 2025 row survives the history window"
        );
    }

    fn plan(
        measures: Vec<Measure>,
        window_days: Option<u64>,
        spark_days: Option<u64>,
    ) -> BoardPlan {
        BoardPlan {
            params: VolBoardParams {
                key: QueryKey(3),
                tag: 1,
                submitted: Instant::now(),
                surface: "vol_surface".into(),
                realized: None,
                varswap: None,
                members: vec!["SPX".into(), "NDX".into()],
                measures,
                lookbacks: vec![Lookback::D1, Lookback::M1],
                window: window_days.map(|d| Duration::from_secs(d * 86_400)),
                sparkline: spark_days
                    .map(|d| (Measure::atfm("3M"), Duration::from_secs(d * 86_400))),
            },
            // Instants fixed by hand (the rule deriving them is in core):
            // the end of Friday the 9th and of Dec 11. `now` is Sunday the
            // 11th at noon, so the daily span ends there.
            now: ts("2026-01-11T12:00:00Z"),
            instants: vec![
                ts("2026-01-09T23:59:59.999999Z"),
                ts("2025-12-11T23:59:59.999999Z"),
            ],
            dataset_errors: Vec::new(),
        }
    }

    #[test]
    fn board_pool_key_differs_from_the_requester_and_round_trips() {
        for k in [0u64, 1, 7, 1 << 40, u64::MAX - 1, u64::MAX] {
            let derived = board_pool_key(QueryKey(k));
            assert_ne!(derived, QueryKey(k));
            assert_eq!(board_pool_key(derived), QueryKey(k));
        }
    }

    #[test]
    fn the_board_reads_current_changes_rank_spark_and_lists_the_missing_member() {
        let (_dir, store, schema) = seeded();
        assert_the_seed_kept_the_superseded_and_the_old_rows(&store);
        let p = plan(
            vec![
                Measure::atfm("3M"),
                Measure::Skew { tenor: "3M".into() },
                Measure::atfm("1M"),
            ],
            Some(365),
            Some(3),
        );
        let board = read_board(store.writer(), &schema, &p).unwrap();
        assert_eq!(board.rows.len(), 2);
        assert_eq!(board.missing, vec!["NDX"]);
        let spx = &board.rows[0];
        assert_eq!(spx.as_of, Some(ts("2026-01-10T21:00:00Z")));
        let atfm = &spx.cells[0];
        assert_eq!(atfm.current, Some(0.20));
        let d1 = atfm.changes[0].as_ref().unwrap();
        assert!(
            (d1.value - 0.01).abs() < 1e-12,
            "the live 0.19 of the 9th, not the superseded 0.99"
        );
        assert_eq!(d1.at, ts("2026-01-09T21:00:00Z"));
        assert_eq!(atfm.changes[1], None);
        let (pctl, n) = atfm.pctl.unwrap();
        assert_eq!(n, 3, "one observation per UTC day: the 10th counts once");
        assert!((pctl - 2.0 / 3.0).abs() < 1e-12);
        let skew = &spx.cells[1];
        assert!((skew.current.unwrap() - 0.05).abs() < 1e-12);
        assert!((skew.changes[0].as_ref().unwrap().value - 0.01).abs() < 1e-12);
        let atfm_1m = &spx.cells[2];
        assert_eq!(
            atfm_1m.current,
            Some(0.17),
            "a stale point still has its current"
        );
        assert_eq!(atfm_1m.pctl, None, "nothing inside the window");
        assert_eq!(spx.spark, Some(vec![0.18, 0.19, 0.20]));
        // The 3M tuples fail M1 and start on the 8th; the 1M tuple answers
        // both lookbacks, so its 2025 row does not set the note.
        assert_eq!(board.notes, vec!["vol_surface: history starts 2026-01-08"]);
        let ndx = &board.rows[1];
        assert_eq!(ndx.as_of, None);
        assert!(ndx.cells.iter().all(|c| c.current.is_none()));
    }

    #[test]
    fn the_daily_bucket_keeps_the_last_mark_of_the_day() {
        let (_dir, store, schema) = seeded();
        let p = plan(vec![Measure::atfm("3M")], None, Some(1));
        let board = read_board(store.writer(), &schema, &p).unwrap();
        // The sparkline's one day is the 10th: the 21:00 close, not the 15:00 mark.
        assert_eq!(board.rows[0].spark, Some(vec![0.20]));
    }

    #[test]
    fn an_instant_equal_to_an_observation_includes_it() {
        let (_dir, store, schema) = seeded();
        let mut p = plan(vec![Measure::atfm("3M")], None, None);
        p.instants = vec![ts("2026-01-09T21:00:00Z"), ts("2026-01-08T20:59:59Z")];
        let board = read_board(store.writer(), &schema, &p).unwrap();
        let cell = &board.rows[0].cells[0];
        assert_eq!(cell.changes[0].as_ref().unwrap().from, 0.19);
        assert_eq!(cell.changes[1], None, "one second before the first close");
    }

    #[test]
    fn the_live_version_wins() {
        let (_dir, store, schema) = seeded();
        assert_the_seed_kept_the_superseded_and_the_old_rows(&store);
        let mut p = plan(vec![Measure::atfm("3M")], None, None);
        p.instants = vec![ts("2026-01-09T21:00:00Z"), ts("2026-01-09T21:00:00Z")];
        let board = read_board(store.writer(), &schema, &p).unwrap();
        assert_eq!(
            board.rows[0].cells[0].changes[0].as_ref().unwrap().from,
            0.19
        );
    }

    #[test]
    fn a_suffix_in_another_spelling_reads_the_stored_key() {
        let (_dir, store, schema) = seeded();
        let p = plan(
            vec![Measure::Point {
                tenor: "3M".into(),
                delta: 0.5,
            }],
            None,
            None,
        );
        // `Measure::inputs` spells 0.5 as "0.5"; a suffix spelt "0.50" by
        // hand must reach the same rows, read back as "0.5", and be
        // recorded under the input as it was asked.
        let mut obs = Observations {
            instants: p.instants.clone(),
            ..Default::default()
        };
        let input = Input {
            role: Role::Surface,
            suffix: vec!["3M".into(), "0.50".into()],
            column: "vol",
        };
        read_role(
            store.writer(),
            &schema,
            &p,
            Role::Surface,
            std::slice::from_ref(&input),
            &mut obs,
        )
        .unwrap();
        let found = obs.by_input.get(&("SPX".to_string(), input)).unwrap();
        assert_eq!(found.current.map(|(_, v)| v), Some(0.20));
    }

    /// A realized history dataset beside the seeded surface: key
    /// `(underlying_ref, window)`, value `vol`.
    fn with_realized(store: &Store, schema: &mut SchemaSpec) {
        let text = r#"
[realized_vol]
family = "history"
key = ["underlying_ref", "window"]
history = "400d"
retention = "7d"
[realized_vol.columns.underlying_ref]
type = "utf8"
role = "dimension"
textual = true
[realized_vol.columns.window]
type = "utf8"
role = "dimension"
[realized_vol.columns.vol]
type = "f64"
role = "value"
"#;
        let doc = geode_core::config::merge_docs(
            "datasets",
            &[geode_core::config::LayerDoc::builtin("datasets", text).unwrap()],
        );
        let (parsed, diags) = SchemaSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        let ds = parsed.dataset("realized_vol").unwrap().clone();
        store.apply_schema(&ds).unwrap();
        schema.datasets.push(ds);
    }

    #[test]
    fn a_dataset_read_failing_partway_contributes_no_observations() {
        let (_dir, store, mut schema) = seeded();
        with_realized(&store, &mut schema);
        // The current and lookback rows read cleanly; the 8th's daily row
        // has a NULL vol, which fails the read after the rows before it
        // (ordered by kind) were already taken.
        store
            .writer()
            .execute_batch(&format!(
                "insert into {table} (underlying_ref, \"window\", ts, received_at, vol) values
                 ('SPX', '20D', TIMESTAMP '2026-01-10 21:00:00', TIMESTAMP '2026-01-10 21:05:00', 0.15),
                 ('SPX', '20D', TIMESTAMP '2026-01-09 21:00:00', TIMESTAMP '2026-01-10 21:05:00', 0.14),
                 ('SPX', '20D', TIMESTAMP '2026-01-08 21:00:00', TIMESTAMP '2026-01-10 21:05:00', NULL)",
                table = history_table("realized_vol")
            ))
            .unwrap();
        let mut p = plan(
            vec![
                Measure::atfm("3M"),
                Measure::IvRv {
                    tenor: "3M".into(),
                    window: "20D".into(),
                },
                Measure::Realized {
                    window: "20D".into(),
                },
            ],
            Some(365),
            None,
        );
        p.params.realized = Some("realized_vol".into());
        let board = read_board(store.writer(), &schema, &p).unwrap();
        assert_eq!(
            board
                .dataset_errors
                .iter()
                .map(|(d, _)| d.as_str())
                .collect::<Vec<_>>(),
            vec!["realized_vol"],
            "{:?}",
            board.dataset_errors
        );
        let spx = &board.rows[0];
        assert_eq!(spx.cells[0].current, Some(0.20), "the surface still reads");
        for cell in &spx.cells[1..] {
            assert_eq!(cell.current, None, "{cell:?}");
            assert!(cell.changes.iter().all(Option::is_none), "{cell:?}");
            assert_eq!(cell.pctl, None, "{cell:?}");
        }
    }

    #[test]
    fn a_member_named_twice_reads_once() {
        let (_dir, store, schema) = seeded();
        let mut p = plan(vec![Measure::atfm("3M")], None, Some(3));
        p.params.members = vec!["SPX".into(), "SPX".into()];
        let board = read_board(store.writer(), &schema, &p).unwrap();
        assert_eq!(board.rows.len(), 2);
        assert!(
            board
                .rows
                .iter()
                .all(|r| r.spark == Some(vec![0.18, 0.19, 0.20]))
        );
    }

    #[test]
    fn an_undeclared_realized_dataset_blanks_its_cells_and_names_itself() {
        let (_dir, store, schema) = seeded();
        let mut p = plan(
            vec![
                Measure::atfm("3M"),
                Measure::IvRv {
                    tenor: "3M".into(),
                    window: "20D".into(),
                },
            ],
            None,
            None,
        );
        p.params.realized = Some("nope".into());
        let board = read_board(store.writer(), &schema, &p).unwrap();
        assert_eq!(
            board.dataset_errors,
            vec![(
                "nope".to_string(),
                "'nope' is not a declared history dataset".to_string()
            )]
        );
        assert_eq!(board.rows[0].cells[0].current, Some(0.20));
        assert_eq!(board.rows[0].cells[1].current, None);
    }

    #[test]
    fn a_dataset_already_in_dataset_errors_is_not_read_again() {
        let (_dir, store, schema) = seeded();
        let mut p = plan(
            vec![
                Measure::atfm("3M"),
                Measure::Realized {
                    window: "20D".into(),
                },
            ],
            None,
            None,
        );
        p.params.realized = Some("nope".into());
        p.dataset_errors = vec![("nope".into(), "realized unavailable".into())];
        let board = read_board(store.writer(), &schema, &p).unwrap();
        assert_eq!(
            board.dataset_errors,
            vec![("nope".to_string(), "realized unavailable".to_string())]
        );
    }

    #[test]
    fn a_surface_read_error_is_the_boards_error() {
        let (_dir, store, schema) = seeded();
        let mut p = plan(vec![Measure::atfm("3M")], None, None);
        p.params.surface = "nope".into();
        let err = read_board(store.writer(), &schema, &p)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("'nope' is not a declared history dataset"),
            "{err}"
        );
    }

    #[test]
    fn a_dataset_with_the_wrong_key_arity_or_missing_column_is_refused() {
        let (_dir, store, mut schema) = seeded();
        let mut p = plan(vec![Measure::atfm("3M")], None, None);
        // Pretend the surface is a two-column-key dataset.
        schema.datasets[0].key.pop();
        let err = read_board(store.writer(), &schema, &p)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("has a 2-column key; the board needs 3"),
            "{err}"
        );
        let (_dir, store, mut schema) = seeded();
        schema.datasets[0].columns.retain(|c| c.name != "forward");
        p.params.measures = vec![Measure::Forward { tenor: "3M".into() }];
        let err = read_board(store.writer(), &schema, &p)
            .unwrap_err()
            .to_string();
        assert!(err.contains("has no value column 'forward'"), "{err}");
    }
}
