//! Compile and execute series queries. Each request produces a points
//! statement across all slots, optional percentile and bin statements per
//! slot, and a coverage statement per source slot.
//!
//! Source points select `arg_max(value, received_at)` per timestamp before
//! bucketing. Historical reads require `ts <= t` and take the latest version
//! with `received_at <= t`, falling back to the earliest version when none
//! was known by then; series do not use generation IDs. Source identities and time bounds use
//! bound parameters; validated numeric literals and bucket intervals form SQL.
//!
//! Statistics repeat the points statement's CTEs and bucketing. All statements
//! execute in one read transaction. See `docs/current/performance.md` for budgets and benchmark guidance.

use crate::store::StoreError;
use crate::store::series::{coverage_table, from_micros, micros, series_table};
use duckdb::types::Value;
use geode_core::query::AsOf;
use geode_core::schema::SchemaSpec;
use geode_core::series::expr::{Ast, Expr, Function, Kind, Op};
use geode_core::series::{
    BucketRule, MAX_BINS, MIN_BINS, SeriesParams, SeriesResult, SlotKind, SlotProvenance,
    SlotResult,
};

#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    pub sql: String,
    pub params: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeriesPlan {
    /// The request's slots, in request order — the order the points
    /// statement's value columns are projected in.
    pub slots: Vec<u8>,
    /// Column 0 is `epoch_us(bucket)` (BIGINT), then one DOUBLE per slot.
    pub points: Statement,
    /// The request's percentile fractions, in request order.
    pub fractions: Vec<f64>,
    /// One statement per slot when `fractions` is non-empty: a single
    /// row, one DOUBLE per fraction (NULL when the window is empty).
    pub percentiles: Vec<(u8, Statement)>,
    /// 0 when density is off.
    pub bin_count: u32,
    /// One statement per slot when density is on: rows of
    /// (lo DOUBLE, hi DOUBLE, k BIGINT in `1..=bin_count`, count BIGINT).
    pub bins: Vec<(u8, Statement)>,
    /// One statement per SOURCE slot: a single row of
    /// (min from_ts, max to_ts, max received_at) as epoch micros, all
    /// nullable — an expression slot has no coverage of its own.
    pub coverage: Vec<(u8, Statement)>,
}

fn refuse(msg: String) -> StoreError {
    StoreError::Series(msg)
}

fn aggregate(rule: BucketRule) -> &'static str {
    match rule {
        BucketRule::Last => "arg_max(v, ts)",
        BucketRule::First => "arg_min(v, ts)",
        BucketRule::Mean => "avg(v)",
        BucketRule::Min => "min(v)",
        BucketRule::Max => "max(v)",
    }
}

/// One expression slot's CTEs: its grid stages `s{n}_0`, `s{n}_1`, …
/// then the final `s{n}`. A stage is added where SQL cannot nest: a
/// window over a window, or a fold over anything but a plain source.
/// Every stage name derives from the slot number, so two slots never
/// collide.
struct Lowering {
    slot: u8,
    ctes: Vec<String>,
    stages: usize,
}

/// The stage chain a series-shaped subtree is lowered on. `name` is
/// its latest stage; a hoist advances it.
struct Grid {
    name: String,
}

/// `(select … )` reading the k-th non-null value of `rel` by bucket
/// order, from the start for `k >= 0` and from the end otherwise; past
/// either end the subquery is NULL.
fn index_sql(rel: &str, k: i64) -> String {
    let (order, offset) = if k >= 0 { ("", k) } else { (" desc", -k - 1) };
    format!("(select v from {rel} where v is not null order by b{order} offset {offset} limit 1)")
}

/// A fold as a scalar subquery over `rel`'s `v`. Every aggregate skips
/// NULL; `count` is cast because the points reader binds every value
/// column as a DOUBLE.
fn fold_sql(f: Function, rel: &str) -> String {
    match f {
        Function::First => index_sql(rel, 0),
        Function::Last => index_sql(rel, -1),
        Function::Min => format!("(select min(v) from {rel})"),
        Function::Max => format!("(select max(v) from {rel})"),
        Function::Mean => format!("(select avg(v) from {rel})"),
        Function::Median => format!("(select quantile_cont(v, 0.5) from {rel})"),
        Function::Std => format!("(select stddev_samp(v) from {rel})"),
        Function::Sum => format!("(select sum(v) from {rel})"),
        Function::Count => format!("(select cast(count(v) as double) from {rel})"),
        _ => unreachable!("fold_sql is called for folds only"),
    }
}

/// Lower one expression slot. `validate` has already run the shape
/// check and refused an expression with no slot.
fn lower_slot(slot: u8, e: &Expr) -> Result<Vec<String>, StoreError> {
    if e.slots().is_empty() {
        return Err(refuse(format!(
            "slot {slot}: an expression must reference at least one slot"
        )));
    }
    let mut l = Lowering {
        slot,
        ctes: Vec::new(),
        stages: 0,
    };
    let mut g = l.open_grid(e);
    let (sql, _) = l.lower(e, &mut g)?;
    l.ctes.push(format!(
        "s{slot} as (\n  select b, {sql} as v\n  from {}\n)",
        g.name
    ));
    Ok(l.ctes)
}

impl Lowering {
    fn stage_name(&mut self) -> String {
        let name = format!("s{}_{}", self.slot, self.stages);
        self.stages += 1;
        name
    }

    /// Stage 0 of a grid for `e`: the inner join of the slots `e` reads
    /// as series, each as a `v{m}` column; or, for a scalar-shaped `e`,
    /// the union of the buckets of every slot it folds, so the flat line
    /// spans them all. A fold's operand never narrows the join.
    fn open_grid(&mut self, e: &Expr) -> Grid {
        let name = self.stage_name();
        let series = e.series_slots();
        let sql = match series.split_first() {
            Some((anchor, rest)) => {
                let cols = series
                    .iter()
                    .map(|m| format!("s{m}.v as v{m}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let joins: String = rest
                    .iter()
                    .map(|d| format!(" join s{d} on s{d}.b = s{anchor}.b"))
                    .collect();
                format!(
                    "{name} as (\n  select s{anchor}.b as b, {cols}\n  from s{anchor}{joins}\n)"
                )
            }
            None => {
                let union = e
                    .slots()
                    .iter()
                    .map(|m| format!("select b from s{m}"))
                    .collect::<Vec<_>>()
                    .join(" union ");
                format!("{name} as (\n  select b from ({union})\n)")
            }
        };
        self.ctes.push(sql);
        Grid { name }
    }

    /// Move `sql`, a value over `g`'s latest stage, into a column of a
    /// new stage and answer the column, so a window can read it: SQL
    /// forbids a window inside a window.
    // The rolling and along-window lowerings are the callers; until
    // they land, this expectation keeps Clippy clean and fails the build
    // the moment the first caller makes it stale.
    #[expect(dead_code, reason = "called by the window lowerings")]
    fn hoist(&mut self, sql: String, g: &mut Grid) -> String {
        let k = self.stages;
        let name = self.stage_name();
        let col = format!("c{k}");
        self.ctes.push(format!(
            "{name} as (\n  select *, {sql} as {col}\n  from {}\n)",
            g.name
        ));
        g.name = name;
        col
    }

    /// The relation a fold or an index reads, with the value as `v`: the
    /// source CTE itself for a plain reference, else a stage holding `x`
    /// lowered on a grid of its own.
    fn fold_rel(&mut self, x: &Expr) -> Result<String, StoreError> {
        if let Ast::Ref(m) = x {
            return Ok(format!("s{m}"));
        }
        let mut g = self.open_grid(x);
        let (sql, _) = self.lower(x, &mut g)?;
        let name = self.stage_name();
        self.ctes.push(format!(
            "{name} as (\n  select b, {sql} as v\n  from {}\n)",
            g.name
        ));
        Ok(name)
    }

    /// `(sql, windowed)`: the SQL for `e` over `g`'s latest stage, and
    /// whether it holds a window function, which a window over it must
    /// hoist. Every binary node is wrapped in exactly one pair of
    /// parentheses, so the tree's shape — not SQL's precedence — decides
    /// what binds to what; a division carries its own zero guard inside
    /// that pair, because a NULL is the only honest answer for a bucket
    /// whose denominator is zero.
    fn lower(&mut self, e: &Expr, g: &mut Grid) -> Result<(String, bool), StoreError> {
        Ok(match e {
            Ast::Ref(m) => (format!("v{m}"), false),
            Ast::Num(x) => {
                if !x.is_finite() {
                    return Err(refuse(format!("literal {x} is not a finite number")));
                }
                (format!("{x:?}"), false)
            }
            Ast::Neg(inner) => {
                let (s, w) = self.lower(inner, g)?;
                (format!("(-({s}))"), w)
            }
            Ast::Bin(op, l, r) => {
                let (l, wl) = self.lower(l, g)?;
                let (r, wr) = self.lower(r, g)?;
                let inner = match op {
                    Op::Add => format!("({l}) + ({r})"),
                    Op::Sub => format!("({l}) - ({r})"),
                    Op::Mul => format!("({l}) * ({r})"),
                    Op::Div => format!("(case when ({r}) = 0 then null else ({l}) / ({r}) end)"),
                };
                (format!("({inner})"), wl || wr)
            }
            Ast::Index(x, k) => {
                let rel = self.fold_rel(x)?;
                (index_sql(&rel, *k), false)
            }
            Ast::Call(f, args) => self.call(*f, args, g)?,
        })
    }

    fn call(
        &mut self,
        f: Function,
        args: &[Expr],
        g: &mut Grid,
    ) -> Result<(String, bool), StoreError> {
        let Some(first) = args.first() else {
            return Err(refuse(format!(
                "slot {}: {} has no argument",
                self.slot,
                f.name()
            )));
        };
        match f.kind() {
            Kind::Fold => Ok((fold_sql(f, &self.fold_rel(first)?), false)),
            Kind::MinMax if args.len() == 1 => Ok((fold_sql(f, &self.fold_rel(first)?), false)),
            Kind::MinMax | Kind::Pointwise | Kind::Along { .. } | Kind::Rolling => {
                let _ = g;
                Err(refuse(format!(
                    "slot {}: {} is not lowered yet",
                    self.slot,
                    f.name()
                )))
            }
        }
    }
}

/// Every refusal the request can earn, in one place and in one order, so
/// the message a trader reads names the first thing wrong rather than
/// whichever check happened to run first. Answers the expression slots in
/// request order, the order the compiler emits their CTEs in: every
/// operand is a source slot, and every source CTE precedes them.
fn validate(schema: &SchemaSpec, params: &SeriesParams) -> Result<Vec<u8>, StoreError> {
    let ds = schema
        .dataset(&params.dataset)
        .ok_or_else(|| refuse(format!("unknown dataset '{}'", params.dataset)))?;
    if !ds.is_series() {
        return Err(refuse(format!(
            "dataset '{}' is not a series dataset",
            ds.name
        )));
    }
    if params.range.0 >= params.range.1 {
        return Err(refuse("range is empty (from must precede to)".into()));
    }
    if params.window.0 < params.range.0
        || params.window.1 > params.range.1
        || params.window.0 > params.window.1
    {
        return Err(refuse("window must lie inside range".into()));
    }
    if params.series.is_empty() {
        return Err(refuse("no slots".into()));
    }
    let mut seen = std::collections::BTreeSet::new();
    for s in &params.series {
        if !seen.insert(s.slot) {
            return Err(refuse(format!("slot {} twice", s.slot)));
        }
    }
    for f in &params.percentiles {
        if !(*f > 0.0 && *f < 1.0) {
            return Err(refuse(format!("percentile {f} is not inside (0, 1)")));
        }
    }
    if let Some(n) = params.bins
        && !(MIN_BINS..=MAX_BINS).contains(&n)
    {
        return Err(refuse(format!(
            "bins {n} is outside {MIN_BINS}..={MAX_BINS}"
        )));
    }
    for s in &params.series {
        if let SlotKind::Expr(e) = &s.kind {
            e.shape()
                .map_err(|m| refuse(format!("slot {}: {m}", s.slot)))?;
            let refs = e.slots();
            if refs.is_empty() {
                return Err(refuse(format!(
                    "slot {}: an expression must reference at least one slot",
                    s.slot
                )));
            }
            if let Some(missing) = refs.iter().find(|r| !seen.contains(r)) {
                return Err(refuse(format!(
                    "slot {} references slot {missing}, which the request lacks",
                    s.slot
                )));
            }
            // An operand that is an expression would need an order over
            // expressions and could close a cycle; the language names
            // source series only, so such a request is malformed.
            if let Some(nested) = refs.iter().find(|r| {
                params
                    .series
                    .iter()
                    .any(|o| o.slot == **r && matches!(o.kind, SlotKind::Expr(_)))
            }) {
                return Err(refuse(format!(
                    "slot {} references slot {nested}, which is not a source",
                    s.slot
                )));
            }
        }
    }
    Ok(params
        .series
        .iter()
        .filter(|s| matches!(s.kind, SlotKind::Expr(_)))
        .map(|s| s.slot)
        .collect())
}

/// The CTE prefix every statement shares, and its bound params in order.
///
/// Source CTEs come first, in request order, each collapsing the
/// bitemporal rows to one value per `ts` before bucketing — the inner
/// `arg_max(value, received_at)` is what makes a corrected point replace
/// its predecessor rather than join it. Expression CTEs follow in request
/// order; their operands are all source CTEs, so every operand a lowering
/// names already exists.
fn ctes(params: &SeriesParams, order: &[u8]) -> Result<(String, Vec<Value>), StoreError> {
    let table = series_table(&params.dataset);
    let interval = params.frequency.interval_sql();
    let mut parts: Vec<String> = Vec::new();
    let mut bound: Vec<Value> = Vec::new();
    // Under an as-of, each `ts` takes its latest version known by the
    // instant, or — when every version arrived later — its FIRST one. A
    // fetch stamps its rows with the fetch time, so a strict
    // `received_at <= t` would hide every bar backfilled after `t`; the
    // fallback keeps corrections honest (one received after `t` still
    // loses to an original known by then) without blanking history.
    let (value, as_of) = match &params.as_of {
        AsOf::Live => ("arg_max(value, received_at)", ""),
        AsOf::At(_) => (
            "coalesce(arg_max(value, received_at) filter (where received_at <= make_timestamp(?)), arg_min(value, received_at))",
            " and ts <= make_timestamp(?)",
        ),
    };
    for s in &params.series {
        if let SlotKind::Source {
            source,
            identity,
            rule,
        } = &s.kind
        {
            parts.push(format!(
                "s{n} as (\n  select time_bucket({interval}, ts) as b, {agg} as v\n  from (\n    select ts, {value} as v\n    from {table}\n    where source = ? and series_id = ? and ts >= make_timestamp(?) and ts < make_timestamp(?){as_of}\n    group by ts\n  )\n  group by b\n)",
                n = s.slot,
                agg = aggregate(*rule),
            ));
            // Placeholders bind in text order: the select list's as-of
            // instant precedes the `where` clause's.
            if let AsOf::At(t) = &params.as_of {
                bound.push(Value::BigInt(micros(*t)));
            }
            bound.push(Value::Text(source.clone()));
            bound.push(Value::Text(identity.clone()));
            bound.push(Value::BigInt(micros(params.range.0)));
            bound.push(Value::BigInt(micros(params.range.1)));
            if let AsOf::At(t) = &params.as_of {
                bound.push(Value::BigInt(micros(*t)));
            }
        }
    }
    for slot in order {
        let spec = params
            .series
            .iter()
            .find(|s| s.slot == *slot)
            .expect("order names request slots");
        let SlotKind::Expr(e) = &spec.kind else {
            unreachable!("order lists expressions only")
        };
        parts.extend(lower_slot(*slot, e)?);
    }
    Ok((format!("with {}", parts.join(",\n")), bound))
}

/// Compile a request without database access, clock reads or mutation.
/// [`run_series`] executes the returned statements on a worker connection.
pub fn compile_series(
    schema: &SchemaSpec,
    params: &SeriesParams,
) -> Result<SeriesPlan, StoreError> {
    let order = validate(schema, params)?;
    let (prefix, bound) = ctes(params, &order)?;
    let slots: Vec<u8> = params.series.iter().map(|s| s.slot).collect();
    let sources: Vec<u8> = params
        .series
        .iter()
        .filter(|s| matches!(s.kind, SlotKind::Source { .. }))
        .map(|s| s.slot)
        .collect();

    // The bucket set is the union of the SOURCE slots' buckets alone: an
    // expression is an inner join of its operands and so can only ever
    // narrow, never widen, the rows a request paints. (`validate`
    // guarantees at least one source slot: every expression references
    // at least one slot, and every slot it references is a source.)
    let buckets = sources
        .iter()
        .map(|n| format!("select b from s{n}"))
        .collect::<Vec<_>>()
        .join(" union ");
    let projection = slots
        .iter()
        .map(|n| format!("s{n}.v"))
        .collect::<Vec<_>>()
        .join(", ");
    let joins: String = slots
        .iter()
        .map(|n| format!(" left join s{n} on s{n}.b = buckets.b"))
        .collect();
    let points = Statement {
        sql: format!(
            "{prefix},\nbuckets as ({buckets})\nselect epoch_us(buckets.b), {projection}\nfrom buckets{joins}\norder by buckets.b"
        ),
        params: bound.clone(),
    };

    let window = [
        Value::BigInt(micros(params.window.0)),
        Value::BigInt(micros(params.window.1)),
    ];
    let mut percentiles = Vec::new();
    if !params.percentiles.is_empty() {
        let cols = params
            .percentiles
            .iter()
            .map(|f| format!("quantile_cont(v, {f:?})"))
            .collect::<Vec<_>>()
            .join(", ");
        for n in &slots {
            let mut p = bound.clone();
            p.extend(window.iter().cloned());
            percentiles.push((
                *n,
                Statement {
                    sql: format!(
                        "{prefix}\nselect {cols} from s{n} where b >= make_timestamp(?) and b < make_timestamp(?)"
                    ),
                    params: p,
                },
            ));
        }
    }

    // The bucket index is spelled out rather than asked of
    // `width_bucket`: the pinned DuckDB has no such scalar function
    // ("Catalog Error: Scalar Function with name width_bucket does not
    // exist!"). It is that function's definition, `m.lo < m.hi` making
    // the division safe: `lo` lands in bin 1, everything but `hi` in
    // `1..k`, and `hi` itself would land in `k + 1`, which is what the
    // `least` folds back into the last bin. The `cast` pins the column
    // to BIGINT whatever `floor` returns, because the reader binds it
    // as one.
    let mut bins = Vec::new();
    if let Some(k) = params.bins {
        for n in &slots {
            let mut p = bound.clone();
            p.extend(window.iter().cloned());
            bins.push((
                *n,
                Statement {
                    sql: format!(
                        "{prefix}\n, w as (select v from s{n} where b >= make_timestamp(?) and b < make_timestamp(?) and v is not null),\n  m as (select min(v) as lo, max(v) as hi from w)\nselect m.lo, m.hi, cast(least(floor((w.v - m.lo) / (m.hi - m.lo) * {k}) + 1, {k}) as bigint) as k, count(*)\nfrom w, m where m.lo < m.hi\ngroup by 1, 2, 3 order by 3"
                    ),
                    params: p,
                },
            ));
        }
    }

    let cov = coverage_table(&params.dataset);
    let coverage = params
        .series
        .iter()
        .filter_map(|s| match &s.kind {
            SlotKind::Source {
                source, identity, ..
            } => Some((
                s.slot,
                Statement {
                    sql: format!(
                        "select epoch_us(min(from_ts)), epoch_us(max(to_ts)), epoch_us(max(received_at)) from {cov} where source = ? and series_id = ?"
                    ),
                    params: vec![Value::Text(source.clone()), Value::Text(identity.clone())],
                },
            )),
            SlotKind::Expr(_) => None,
        })
        .collect();

    Ok(SeriesPlan {
        slots,
        points,
        fractions: params.percentiles.clone(),
        percentiles,
        bin_count: params.bins.unwrap_or(0),
        bins,
        coverage,
    })
}

/// A NULL value column is a bucket this slot has no point in, and
/// `SlotResult::values` is a dense `Vec<f64>` the length of `buckets` —
/// so the gap has to be a float. `NaN` is that float, as that field's
/// own contract says: a gap must not arrive as a zero a chart would
/// draw, nor as a number any later arithmetic could absorb.
fn nan_if_null(v: Option<f64>) -> f64 {
    v.unwrap_or(f64::NAN)
}

/// Run one compiled plan on a connection. The points statement is read
/// once into a column per slot, then each slot's stats and coverage
/// statements follow in the same read transaction. A concurrent publication
/// cannot make points, statistics and coverage describe different snapshots.
pub fn run_series(
    conn: &duckdb::Connection,
    plan: &SeriesPlan,
) -> Result<SeriesResult, duckdb::Error> {
    run_series_with(conn, plan, || {})
}

fn run_series_with(
    conn: &duckdb::Connection,
    plan: &SeriesPlan,
    after_points: impl FnOnce(),
) -> Result<SeriesResult, duckdb::Error> {
    let tx = conn.unchecked_transaction()?;
    let conn = &tx;
    let k = plan.slots.len();
    let mut buckets: Vec<i64> = Vec::new();
    let mut values: Vec<Vec<f64>> = vec![Vec::new(); k];
    {
        let mut stmt = conn.prepare(&plan.points.sql)?;
        let mut rows = stmt.query(duckdb::params_from_iter(plan.points.params.iter()))?;
        while let Some(row) = rows.next()? {
            buckets.push(row.get::<_, i64>(0)?);
            for (i, col) in values.iter_mut().enumerate() {
                col.push(nan_if_null(row.get::<_, Option<f64>>(i + 1)?));
            }
        }
    }

    after_points();
    let mut slots = Vec::with_capacity(k);
    for (i, slot) in plan.slots.iter().enumerate() {
        // One NULL means the window held nothing at all, so the slot
        // reports no percentiles rather than a partial list.
        let mut percentiles = Vec::new();
        if let Some((_, st)) = plan.percentiles.iter().find(|(s, _)| s == slot) {
            let mut stmt = conn.prepare(&st.sql)?;
            let mut rows = stmt.query(duckdb::params_from_iter(st.params.iter()))?;
            if let Some(row) = rows.next()? {
                for (j, f) in plan.fractions.iter().enumerate() {
                    match row.get::<_, Option<f64>>(j)? {
                        Some(v) => percentiles.push((*f, v)),
                        None => {
                            percentiles.clear();
                            break;
                        }
                    }
                }
            }
        }

        // The statement emits only the non-empty bins; the zero-count
        // ones in between are filled here, and the edges come from the
        // `(lo, hi)` every row repeats. No rows at all means the window
        // held fewer than two distinct values, and there are no bins.
        let mut bins = Vec::new();
        if let Some((_, st)) = plan.bins.iter().find(|(s, _)| s == slot) {
            let n = plan.bin_count as usize;
            let mut stmt = conn.prepare(&st.sql)?;
            let mut rows = stmt.query(duckdb::params_from_iter(st.params.iter()))?;
            let mut counts = vec![0u32; n];
            let mut edges: Option<(f64, f64)> = None;
            while let Some(row) = rows.next()? {
                let lo: f64 = row.get(0)?;
                let hi: f64 = row.get(1)?;
                let kk: i64 = row.get(2)?;
                let c: i64 = row.get(3)?;
                edges = Some((lo, hi));
                if (1..=n as i64).contains(&kk) {
                    counts[(kk - 1) as usize] = c as u32;
                }
            }
            if let Some((lo, hi)) = edges {
                let w = (hi - lo) / n as f64;
                bins = counts
                    .iter()
                    .enumerate()
                    .map(|(b, c)| (lo + b as f64 * w, lo + (b as f64 + 1.0) * w, *c))
                    .collect();
            }
        }

        // Expression slots have no coverage statement and retain empty provenance.
        // The service attaches source-slot health after query execution.
        let mut provenance = SlotProvenance {
            loaded: None,
            latest_received_at: None,
            health: None,
        };
        if let Some((_, st)) = plan.coverage.iter().find(|(s, _)| s == slot) {
            let mut stmt = conn.prepare(&st.sql)?;
            let mut rows = stmt.query(duckdb::params_from_iter(st.params.iter()))?;
            if let Some(row) = rows.next()? {
                let from: Option<i64> = row.get(0)?;
                let to: Option<i64> = row.get(1)?;
                let latest: Option<i64> = row.get(2)?;
                if let (Some(f), Some(t)) = (from, to) {
                    provenance.loaded = Some((from_micros(f), from_micros(t)));
                }
                provenance.latest_received_at = latest.map(from_micros);
            }
        }

        slots.push(SlotResult {
            slot: *slot,
            values: std::mem::take(&mut values[i]),
            percentiles,
            bins,
            provenance,
        });
    }
    tx.commit()?;
    Ok(SeriesResult { buckets, slots })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ddl::tests_support::{cvi_dataset, series_dataset, ts};
    use duckdb::types::Value;
    use geode_core::query::{AsOf, QueryKey};
    use geode_core::series::expr::{RefName, parse};
    use geode_core::series::{BucketRule, Frequency, SeriesParams, SeriesSpec, SlotKind};

    pub(super) fn schema() -> SchemaSpec {
        let mut s = SchemaSpec::default();
        s.datasets.push(series_dataset());
        s.datasets.push(cvi_dataset());
        s
    }

    pub(super) fn source(slot: u8, identity: &str, rule: BucketRule) -> SeriesSpec {
        SeriesSpec {
            slot,
            kind: SlotKind::Source {
                source: "demo_kdb".into(),
                identity: identity.into(),
                rule,
            },
        }
    }

    /// Resolves `sN`-shaped names to slot N: the tests' shorthand for
    /// a tile whose series happen to be named so.
    pub(super) fn by_s_number(r: &RefName) -> Option<u8> {
        r.identity.strip_prefix('s')?.parse().ok()
    }

    pub(super) fn expr(slot: u8, text: &str) -> SeriesSpec {
        let e = parse(text).unwrap().resolve(&mut by_s_number).unwrap();
        SeriesSpec {
            slot,
            kind: SlotKind::Expr(e),
        }
    }

    pub(super) fn params(series: Vec<SeriesSpec>) -> SeriesParams {
        SeriesParams {
            key: QueryKey(7),
            tag: 1,
            submitted: std::time::Instant::now(),
            dataset: "series".into(),
            range: (ts("2026-01-05T00:00:00Z"), ts("2026-01-10T00:00:00Z")),
            window: (ts("2026-01-06T00:00:00Z"), ts("2026-01-09T00:00:00Z")),
            as_of: AsOf::Live,
            frequency: Frequency::D1,
            series,
            percentiles: Vec::new(),
            bins: None,
        }
    }

    fn micros(s: &str) -> Value {
        Value::BigInt(crate::store::series::micros(ts(s)))
    }

    #[test]
    fn a_live_source_slot_buckets_the_live_rows_with_its_rule() {
        let plan = compile_series(
            &schema(),
            &params(vec![source(1, "SPX.close", BucketRule::Last)]),
        )
        .unwrap();
        let sql = &plan.points.sql;
        assert!(
            sql.contains("time_bucket(interval '1 day', ts) as b, arg_max(v, ts) as v"),
            "{sql}"
        );
        assert!(
            sql.contains("select ts, arg_max(value, received_at) as v"),
            "{sql}"
        );
        assert!(sql.contains("from series_series"), "{sql}");
        assert!(
            sql.contains(
                "where source = ? and series_id = ? and ts >= make_timestamp(?) and ts < make_timestamp(?)"
            ),
            "{sql}"
        );
        assert!(
            !sql.contains("received_at <="),
            "live has no as-of predicate: {sql}"
        );
        assert!(sql.contains("buckets as (select b from s1)"), "{sql}");
        assert!(
            sql.trim_end().ends_with(
                "select epoch_us(buckets.b), s1.v\nfrom buckets left join s1 on s1.b = buckets.b\norder by buckets.b"
            ),
            "{sql}"
        );
        assert_eq!(
            plan.points.params,
            vec![
                Value::Text("demo_kdb".into()),
                Value::Text("SPX.close".into()),
                micros("2026-01-05T00:00:00Z"),
                micros("2026-01-10T00:00:00Z")
            ]
        );
        assert_eq!(plan.slots, vec![1]);
        assert!(plan.percentiles.is_empty() && plan.bins.is_empty());
        assert_eq!(plan.coverage.len(), 1);
        assert!(
            plan.coverage[0]
                .1
                .sql
                .contains("from series_series_coverage where source = ? and series_id = ?"),
            "{}",
            plan.coverage[0].1.sql
        );
    }

    #[test]
    fn every_rule_maps_to_its_aggregate() {
        for (rule, agg) in [
            (BucketRule::Last, "arg_max(v, ts) as v"),
            (BucketRule::First, "arg_min(v, ts) as v"),
            (BucketRule::Mean, "avg(v) as v"),
            (BucketRule::Min, "min(v) as v"),
            (BucketRule::Max, "max(v) as v"),
        ] {
            let plan = compile_series(&schema(), &params(vec![source(1, "X", rule)])).unwrap();
            assert!(
                plan.points.sql.contains(agg),
                "{rule:?}: {}",
                plan.points.sql
            );
        }
    }

    #[test]
    fn an_as_of_filters_received_at_and_ts_with_two_bound_copies_of_the_instant() {
        let mut p = params(vec![source(1, "X", BucketRule::Last)]);
        p.as_of = AsOf::At(ts("2026-01-08T12:00:00Z"));
        let plan = compile_series(&schema(), &p).unwrap();
        assert!(
            plan.points.sql.contains(
                "select ts, coalesce(arg_max(value, received_at) filter (where received_at <= make_timestamp(?)), arg_min(value, received_at)) as v"
            ),
            "{}",
            plan.points.sql
        );
        assert!(
            plan.points
                .sql
                .contains("and ts < make_timestamp(?) and ts <= make_timestamp(?)"),
            "{}",
            plan.points.sql
        );
        assert_eq!(plan.points.params.len(), 6);
        assert_eq!(plan.points.params[0], micros("2026-01-08T12:00:00Z"));
        assert_eq!(plan.points.params[5], micros("2026-01-08T12:00:00Z"));
    }

    #[test]
    fn an_expression_is_an_inner_join_of_its_operands_with_a_guarded_division() {
        let plan = compile_series(
            &schema(),
            &params(vec![
                source(1, "A", BucketRule::Last),
                source(2, "B", BucketRule::Last),
                expr(3, "s1 / s2"),
            ]),
        )
        .unwrap();
        let sql = &plan.points.sql;
        assert!(
            sql.contains(
                "s3_0 as (\n  select s1.b as b, s1.v as v1, s2.v as v2\n  from s1 join s2 on s2.b = s1.b\n)"
            ),
            "{sql}"
        );
        assert!(
            sql.contains(
                "s3 as (\n  select b, ((case when (v2) = 0 then null else (v1) / (v2) end)) as v\n  from s3_0\n)"
            ),
            "{sql}"
        );
        assert!(
            sql.contains("buckets as (select b from s1 union select b from s2)"),
            "expressions never widen the bucket set: {sql}"
        );
        assert!(
            sql.contains("select epoch_us(buckets.b), s1.v, s2.v, s3.v\n"),
            "{sql}"
        );
        assert!(sql.contains("left join s3 on s3.b = buckets.b"), "{sql}");
        assert_eq!(plan.slots, vec![1, 2, 3]);
        assert_eq!(
            plan.coverage.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec![1, 2],
            "no coverage for an expression"
        );
    }

    #[test]
    fn expressions_are_emitted_after_every_source_whatever_the_request_order() {
        let plan = compile_series(
            &schema(),
            &params(vec![
                expr(4, "s2 - s1"),
                source(1, "A", BucketRule::Last),
                expr(3, "s1 * 2"),
                source(2, "B", BucketRule::Mean),
            ]),
        )
        .unwrap();
        let sql = &plan.points.sql;
        let (s1, s2, s3, s4) = (
            sql.find("s1 as (").unwrap(),
            sql.find("s2 as (").unwrap(),
            sql.find("s3 as (").unwrap(),
            sql.find("s4 as (").unwrap(),
        );
        assert!(s1 < s4 && s2 < s4 && s1 < s3 && s2 < s3, "{sql}");
        assert!(
            sql.contains("s3_0 as (\n  select s1.b as b, s1.v as v1\n  from s1\n)")
                && sql.contains("s3 as (\n  select b, ((v1) * (2.0)) as v\n  from s3_0\n)"),
            "{sql}"
        );
        assert!(
            sql.contains(
                "s4_0 as (\n  select s1.b as b, s1.v as v1, s2.v as v2\n  from s1 join s2 on s2.b = s1.b\n)"
            ) && sql.contains("s4 as (\n  select b, ((v2) - (v1)) as v\n  from s4_0\n)"),
            "{sql}"
        );
        assert!(
            sql.contains("select epoch_us(buckets.b), s4.v, s1.v, s3.v, s2.v\n"),
            "the projection is in REQUEST order: {sql}"
        );
        assert_eq!(plan.slots, vec![4, 1, 3, 2]);
    }

    #[test]
    fn negation_and_nesting_lower_with_parentheses() {
        let plan = compile_series(
            &schema(),
            &params(vec![
                source(1, "A", BucketRule::Last),
                expr(2, "-(s1 + 1.5) * s1"),
            ]),
        )
        .unwrap();
        assert!(
            plan.points
                .sql
                .contains("(((-(((v1) + (1.5))))) * (v1)) as v"),
            "{}",
            plan.points.sql
        );
    }

    #[test]
    fn percentiles_and_bins_are_one_statement_per_slot_over_the_window() {
        let mut p = params(vec![source(1, "A", BucketRule::Last), expr(2, "s1 * 2")]);
        p.percentiles = vec![0.05, 0.5, 0.95];
        p.bins = Some(40);
        let plan = compile_series(&schema(), &p).unwrap();
        assert_eq!(plan.fractions, vec![0.05, 0.5, 0.95]);
        assert_eq!(plan.bin_count, 40);
        assert_eq!(plan.percentiles.len(), 2);
        let (slot, st) = &plan.percentiles[1];
        assert_eq!(*slot, 2);
        assert!(
            st.sql.ends_with(
                "select quantile_cont(v, 0.05), quantile_cont(v, 0.5), quantile_cont(v, 0.95) from s2 where b >= make_timestamp(?) and b < make_timestamp(?)"
            ),
            "{}",
            st.sql
        );
        assert!(
            st.sql.contains("s1 as ("),
            "the stats statements carry the CTEs: {}",
            st.sql
        );
        let n = st.params.len();
        assert_eq!(
            &st.params[n - 2..],
            &[
                micros("2026-01-06T00:00:00Z"),
                micros("2026-01-09T00:00:00Z")
            ]
        );
        let (slot, st) = &plan.bins[0];
        assert_eq!(*slot, 1);
        assert!(
            st.sql.contains(
                ", w as (select v from s1 where b >= make_timestamp(?) and b < make_timestamp(?) and v is not null)"
            ),
            "{}",
            st.sql
        );
        assert!(
            st.sql.contains(
                "cast(least(floor((w.v - m.lo) / (m.hi - m.lo) * 40) + 1, 40) as bigint) as k"
            ),
            "{}",
            st.sql
        );
        assert!(st.sql.contains("where m.lo < m.hi"), "{}", st.sql);
    }

    #[test]
    fn every_refusal_names_its_reason() {
        let s = schema();
        let refuse = |p: SeriesParams| compile_series(&s, &p).unwrap_err().to_string();
        let mut p = params(vec![source(1, "A", BucketRule::Last)]);
        p.dataset = "nope".into();
        assert!(refuse(p).contains("unknown dataset 'nope'"));
        let mut p = params(vec![source(1, "A", BucketRule::Last)]);
        p.dataset = "cvi_params".into();
        assert!(refuse(p).contains("not a series dataset"));
        let mut p = params(vec![source(1, "A", BucketRule::Last)]);
        p.range = (ts("2026-01-10T00:00:00Z"), ts("2026-01-05T00:00:00Z"));
        assert!(refuse(p).contains("range"));
        let mut p = params(vec![source(1, "A", BucketRule::Last)]);
        p.window = (ts("2026-01-01T00:00:00Z"), ts("2026-01-09T00:00:00Z"));
        assert!(refuse(p).contains("window"));
        assert!(refuse(params(vec![])).contains("no slots"));
        assert!(
            refuse(params(vec![
                source(1, "A", BucketRule::Last),
                source(1, "B", BucketRule::Last)
            ]))
            .contains("slot 1 twice")
        );
        let mut p = params(vec![source(1, "A", BucketRule::Last)]);
        p.percentiles = vec![0.5, 1.0];
        assert!(refuse(p).contains("percentile"));
        let mut p = params(vec![source(1, "A", BucketRule::Last)]);
        p.bins = Some(3);
        assert!(refuse(p).contains("bins"));
        let e = refuse(params(vec![
            source(1, "A", BucketRule::Last),
            expr(2, "s1 + s9"),
        ]));
        assert!(e.contains("slot 2") && e.contains("slot 9"), "{e}");
        assert!(refuse(params(vec![expr(2, "2 + 3")])).contains("reference"));
        let e = refuse(params(vec![
            source(1, "A", BucketRule::Last),
            expr(2, "mean(2) + s1"),
        ]));
        assert!(
            e.contains("slot 2") && e.contains("mean needs a series"),
            "{e}"
        );
    }

    /// An expression names source series only; an operand that is
    /// itself an expression (a self-reference included) is refused by
    /// name rather than ordered, so no request can carry a cycle.
    #[test]
    fn an_expression_over_an_expression_is_refused() {
        let s = schema();
        let refuse = |p: SeriesParams| compile_series(&s, &p).unwrap_err().to_string();
        let e = refuse(params(vec![
            source(1, "A", BucketRule::Last),
            expr(2, "s1 * 2"),
            expr(3, "s2 + s1"),
        ]));
        assert!(
            e.contains("slot 3") && e.contains("slot 2") && e.contains("not a source"),
            "{e}"
        );
        let e = refuse(params(vec![
            source(1, "A", BucketRule::Last),
            expr(2, "s2 + s1"),
        ]));
        assert!(e.contains("slot 2 references slot 2"), "{e}");
    }

    /// A literal too big for an `f64` is not a parse error: Rust's own
    /// `str::parse` answers `Ok(inf)` on overflow, so a pasted wall of
    /// digits arrives at the compiler as `Ast::Num(inf)`. `lower`'s
    /// finiteness check is the only thing between that and the text
    /// `inf` inside a SQL string, which DuckDB would refuse at prepare
    /// time as a syntax error naming an identifier a trader never typed.
    #[test]
    fn a_non_finite_literal_is_refused() {
        let text = format!("s1 * {}", "9".repeat(400));
        let ast = parse(&text).unwrap();
        assert!(
            matches!(&ast, Ast::Bin(_, _, r) if matches!(**r, Ast::Num(x) if !x.is_finite())),
            "the literal overflows to inf rather than failing to parse: {ast:?}"
        );
        let e = ast.resolve(&mut by_s_number).unwrap();
        let err = compile_series(
            &schema(),
            &params(vec![
                source(1, "A", BucketRule::Last),
                SeriesSpec {
                    slot: 2,
                    kind: SlotKind::Expr(e),
                },
            ]),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("finite"), "{err}");
    }

    // The end-to-end half: every statement above run against a real
    // DuckDB store, so a plan that reads plausibly but answers wrongly
    // has nowhere to hide.

    use crate::adapter::SeriesRows;
    use crate::store::Store;
    use crate::store::catalog::Catalog;
    use crate::store::series::{SeriesAppendRequest, append_series};
    use geode_core::series::SeriesResult;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        store.apply_schema(&series_dataset()).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        (dir, store)
    }

    /// `values` at one-minute steps from `start`, appended for `identity`
    /// under `demo_kdb` with the given `received_at`.
    fn append(store: &Store, identity: &str, start: &str, values: &[f64], received: &str) {
        let start_t = ts(start);
        let rows = SeriesRows {
            ts: (0..values.len())
                .map(|i| start_t + chrono::Duration::minutes(i as i64))
                .collect(),
            value: values.to_vec(),
        };
        let ds = series_dataset();
        append_series(
            store,
            &SeriesAppendRequest {
                dataset: &ds,
                source: "demo_kdb",
                identity,
                rows: &rows,
                span: (start_t, start_t + chrono::Duration::days(1)),
                received_at: ts(received),
            },
        )
        .unwrap();
    }

    fn run(store: &Store, p: &SeriesParams) -> SeriesResult {
        let plan = compile_series(&schema(), p).unwrap();
        run_series(store.writer(), &plan).unwrap()
    }

    fn nan_or(v: f64) -> Option<f64> {
        if v.is_nan() { None } else { Some(v) }
    }

    /// One point per day from Jan 5 for `identity`: `values[i]` on
    /// Jan 5 + i, a NaN being a day with no point (a gap, not a NULL).
    fn daily(store: &Store, identity: &str, values: &[f64]) {
        for (i, v) in values.iter().enumerate() {
            if v.is_nan() {
                continue;
            }
            let day = ts("2026-01-05T14:30:00Z") + chrono::Duration::days(i as i64);
            append(
                store,
                identity,
                &day.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                &[*v],
                "2026-01-20T09:00:00Z",
            );
        }
    }

    /// Slot `i` of the result as `Some(value)` per bucket, `None` for a gap.
    fn vals(r: &SeriesResult, i: usize) -> Vec<Option<f64>> {
        r.slots[i].values.iter().map(|v| nan_or(*v)).collect()
    }

    fn close(a: &[Option<f64>], b: &[Option<f64>]) -> bool {
        a.len() == b.len()
            && a.iter().zip(b).all(|(x, y)| match (x, y) {
                (None, None) => true,
                (Some(x), Some(y)) => (x - y).abs() < 1e-9,
                _ => false,
            })
    }

    #[test]
    fn folds_and_indexes_read_the_whole_range_and_skip_gaps() {
        let (_d, store) = store();
        // Points on Jan 5, 6, 8, 9; Jan 7 is a gap.
        daily(&store, "A", &[2.0, 4.0, f64::NAN, 8.0, 6.0]);
        let r = run(
            &store,
            &params(vec![
                source(1, "A", BucketRule::Last),
                expr(2, "s1 / s1[0]"),
                expr(3, "s1 / s1[-1]"),
                expr(4, "mean(s1)"),
                expr(5, "s1[2] + s1[-3]"),
                expr(6, "s1[4]"),
                expr(7, "s1[999999] + s1[-999999]"),
                expr(
                    8,
                    "first(s1) + last(s1) + min(s1) + max(s1) + sum(s1) + count(s1) + median(s1)",
                ),
                expr(9, "std(s1)"),
                expr(10, "count(s1)"),
            ]),
        );
        assert_eq!(
            r.buckets.len(),
            4,
            "A's four buckets; a scalar line never widens them"
        );
        let line = |v: f64| vec![Some(v); 4];
        assert!(
            close(&vals(&r, 1), &[Some(1.0), Some(2.0), Some(4.0), Some(3.0)]),
            "{:?}",
            vals(&r, 1)
        );
        assert!(
            close(
                &vals(&r, 2),
                &[Some(1.0 / 3.0), Some(2.0 / 3.0), Some(4.0 / 3.0), Some(1.0)]
            ),
            "{:?}",
            vals(&r, 2)
        );
        assert!(
            close(&vals(&r, 3), &line(5.0)),
            "a flat line over A's buckets: {:?}",
            vals(&r, 3)
        );
        assert!(
            close(&vals(&r, 4), &line(12.0)),
            "[2] from the start is 8, [-3] from the end is 4: {:?}",
            vals(&r, 4)
        );
        assert_eq!(vals(&r, 5), vec![None; 4], "past the end is a gap");
        assert_eq!(
            vals(&r, 6),
            vec![None; 4],
            "far past either end is a gap, not an error"
        );
        assert!(
            close(&vals(&r, 7), &line(47.0)),
            "2 + 6 + 2 + 8 + 20 + 4 + 5: {:?}",
            vals(&r, 7)
        );
        assert!(
            close(&vals(&r, 8), &line((20.0f64 / 3.0).sqrt())),
            "sample deviation of 2, 4, 8, 6: {:?}",
            vals(&r, 8)
        );
        assert!(
            close(&vals(&r, 9), &line(4.0)),
            "a count reaches the reader as a double: {:?}",
            vals(&r, 9)
        );
    }

    #[test]
    fn a_scalar_line_spans_the_union_of_the_series_it_folds_and_a_fold_never_narrows() {
        let (_d, store) = store();
        daily(&store, "A", &[1.0, 2.0, f64::NAN, f64::NAN, f64::NAN]);
        daily(&store, "B", &[f64::NAN, f64::NAN, 10.0, 20.0, 30.0]);
        let r = run(
            &store,
            &params(vec![
                source(1, "A", BucketRule::Last),
                source(2, "B", BucketRule::Last),
                expr(3, "mean(s1) + mean(s2)"),
                expr(4, "s1 * last(s2)"),
            ]),
        );
        assert_eq!(r.buckets.len(), 5);
        assert!(
            close(&vals(&r, 2), &[Some(21.5); 5]),
            "over A's and B's buckets: {:?}",
            vals(&r, 2)
        );
        assert_eq!(
            vals(&r, 3),
            vec![Some(30.0), Some(60.0), None, None, None],
            "B is folded, so it does not narrow A's buckets"
        );
    }

    #[test]
    fn daily_buckets_apply_every_rule_over_the_live_rows() {
        let (_d, store) = store();
        // Jan 5: 1,2,3,4 (minutes 14:30..14:33); Jan 6: 10,20
        append(
            &store,
            "A",
            "2026-01-05T14:30:00Z",
            &[1.0, 2.0, 3.0, 4.0],
            "2026-01-06T09:00:00Z",
        );
        append(
            &store,
            "A",
            "2026-01-06T14:30:00Z",
            &[10.0, 20.0],
            "2026-01-07T09:00:00Z",
        );
        for (rule, day1, day2) in [
            (BucketRule::Last, 4.0, 20.0),
            (BucketRule::First, 1.0, 10.0),
            (BucketRule::Mean, 2.5, 15.0),
            (BucketRule::Min, 1.0, 10.0),
            (BucketRule::Max, 4.0, 20.0),
        ] {
            let r = run(&store, &params(vec![source(1, "A", rule)]));
            assert_eq!(
                r.buckets,
                vec![
                    crate::store::series::micros(ts("2026-01-05T00:00:00Z")),
                    crate::store::series::micros(ts("2026-01-06T00:00:00Z"))
                ],
                "{rule:?}"
            );
            assert_eq!(r.slots.len(), 1);
            assert_eq!(r.slots[0].slot, 1);
            assert_eq!(r.slots[0].values, vec![day1, day2], "{rule:?}");
        }
    }

    #[test]
    fn the_bucket_set_is_the_union_and_a_missing_bucket_is_nan() {
        let (_d, store) = store();
        append(
            &store,
            "A",
            "2026-01-05T14:30:00Z",
            &[1.0],
            "2026-01-06T09:00:00Z",
        );
        append(
            &store,
            "A",
            "2026-01-07T14:30:00Z",
            &[3.0],
            "2026-01-08T09:00:00Z",
        );
        append(
            &store,
            "B",
            "2026-01-06T14:30:00Z",
            &[5.0],
            "2026-01-07T09:00:00Z",
        );
        append(
            &store,
            "B",
            "2026-01-07T14:30:00Z",
            &[7.0],
            "2026-01-08T09:00:00Z",
        );
        let r = run(
            &store,
            &params(vec![
                source(1, "A", BucketRule::Last),
                source(2, "B", BucketRule::Last),
                expr(3, "s1 + s2"),
            ]),
        );
        assert_eq!(r.buckets.len(), 3, "Jan 5, 6, 7");
        let a: Vec<Option<f64>> = r.slots[0].values.iter().copied().map(nan_or).collect();
        let b: Vec<Option<f64>> = r.slots[1].values.iter().copied().map(nan_or).collect();
        let e: Vec<Option<f64>> = r.slots[2].values.iter().copied().map(nan_or).collect();
        assert_eq!(a, vec![Some(1.0), None, Some(3.0)]);
        assert_eq!(b, vec![None, Some(5.0), Some(7.0)]);
        assert_eq!(
            e,
            vec![None, None, Some(10.0)],
            "an expression exists only where every operand does"
        );
    }

    #[test]
    fn a_zero_denominator_is_a_gap_not_an_infinity() {
        let (_d, store) = store();
        append(
            &store,
            "A",
            "2026-01-05T14:30:00Z",
            &[6.0],
            "2026-01-06T09:00:00Z",
        );
        append(
            &store,
            "B",
            "2026-01-05T14:30:00Z",
            &[0.0],
            "2026-01-06T09:00:00Z",
        );
        let r = run(
            &store,
            &params(vec![
                source(1, "A", BucketRule::Last),
                source(2, "B", BucketRule::Last),
                expr(3, "s1 / s2"),
            ]),
        );
        assert!(r.slots[2].values[0].is_nan());
    }

    #[test]
    fn an_as_of_before_a_correction_sees_the_original_value() {
        let (_d, store) = store();
        append(
            &store,
            "A",
            "2026-01-05T14:30:00Z",
            &[100.0],
            "2026-01-06T09:00:00Z",
        );
        append(
            &store,
            "A",
            "2026-01-05T14:30:00Z",
            &[101.0],
            "2026-01-07T09:00:00Z",
        );
        let live = run(&store, &params(vec![source(1, "A", BucketRule::Last)]));
        assert_eq!(live.slots[0].values, vec![101.0]);
        let mut p = params(vec![source(1, "A", BucketRule::Last)]);
        p.as_of = AsOf::At(ts("2026-01-06T12:00:00Z"));
        let then = run(&store, &p);
        assert_eq!(then.slots[0].values, vec![100.0]);
        // and an as-of before the bar's own ts hides it entirely
        p.as_of = AsOf::At(ts("2026-01-05T12:00:00Z"));
        let earlier = run(&store, &p);
        assert!(earlier.buckets.is_empty());
    }

    #[test]
    fn an_as_of_before_a_bar_was_fetched_still_sees_its_first_version() {
        // A backfill fetched on Jan 9 stamps every bar received Jan 9; an
        // as-of of Jan 7 must still paint Jan 5 and 6 (not Jan 8, past
        // the as-of's own ts bound), each at its FIRST known value.
        let (_d, store) = store();
        for (day, v) in [("05", 1.0), ("06", 2.0), ("08", 4.0)] {
            append(
                &store,
                "A",
                &format!("2026-01-{day}T14:30:00Z"),
                &[v],
                "2026-01-09T09:00:00Z",
            );
        }
        append(
            &store,
            "A",
            "2026-01-06T14:30:00Z",
            &[20.0],
            "2026-01-10T09:00:00Z",
        );
        let mut p = params(vec![source(1, "A", BucketRule::Last)]);
        p.as_of = AsOf::At(ts("2026-01-07T12:00:00Z"));
        let then = run(&store, &p);
        assert_eq!(then.slots[0].values, vec![1.0, 2.0]);
        let live = run(&store, &params(vec![source(1, "A", BucketRule::Last)]));
        assert_eq!(live.slots[0].values, vec![1.0, 20.0, 4.0]);
    }

    #[test]
    fn percentiles_and_bins_are_computed_over_the_window_only() {
        let (_d, store) = store();
        // Jan 5 (outside the window): 1000. Jan 6..8 (inside, 1m
        // buckets): 1..=8 at 14:30..14:37 on Jan 6.
        append(
            &store,
            "A",
            "2026-01-05T14:30:00Z",
            &[1000.0],
            "2026-01-06T09:00:00Z",
        );
        append(
            &store,
            "A",
            "2026-01-06T14:30:00Z",
            &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0],
            "2026-01-07T09:00:00Z",
        );
        let mut p = params(vec![source(1, "A", BucketRule::Last)]);
        p.frequency = Frequency::M1;
        p.percentiles = vec![0.5];
        p.bins = Some(4);
        let r = run(&store, &p);
        assert_eq!(
            r.slots[0].percentiles,
            vec![(0.5, 4.5)],
            "the 1000 outside the window is not counted"
        );
        let bins = &r.slots[0].bins;
        assert_eq!(bins.len(), 4);
        assert_eq!(
            bins.iter().map(|b| b.2).collect::<Vec<_>>(),
            vec![2, 2, 2, 2],
            "{bins:?}"
        );
        assert!(
            (bins[0].0 - 1.0).abs() < 1e-9 && (bins[3].1 - 8.0).abs() < 1e-9,
            "{bins:?}"
        );
        assert!((bins[1].0 - bins[0].1).abs() < 1e-9, "bins are contiguous");
        // Two fractions, so that column j being fraction j is pinned by
        // a value and not only by the one-column case.
        p.percentiles = vec![0.25, 0.75];
        let r = run(&store, &p);
        assert_eq!(
            r.slots[0].percentiles,
            vec![(0.25, 2.75), (0.75, 6.25)],
            "each fraction reads its own column"
        );
    }

    #[test]
    fn an_empty_window_has_no_stats_and_a_constant_series_has_no_bins() {
        let (_d, store) = store();
        append(
            &store,
            "A",
            "2026-01-05T14:30:00Z",
            &[5.0, 5.0, 5.0],
            "2026-01-06T09:00:00Z",
        );
        let mut p = params(vec![source(1, "A", BucketRule::Last)]);
        p.frequency = Frequency::M1;
        p.percentiles = vec![0.5];
        p.bins = Some(4);
        p.window = (ts("2026-01-08T00:00:00Z"), ts("2026-01-09T00:00:00Z"));
        let r = run(&store, &p);
        assert!(r.slots[0].percentiles.is_empty());
        assert!(r.slots[0].bins.is_empty());
        p.window = (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z"));
        let r = run(&store, &p);
        assert_eq!(r.slots[0].percentiles, vec![(0.5, 5.0)]);
        assert!(r.slots[0].bins.is_empty(), "fewer than two distinct values");
    }

    #[test]
    fn provenance_carries_the_coverage_hull_for_a_source_slot_and_nothing_for_an_expression() {
        let (_d, store) = store();
        append(
            &store,
            "A",
            "2026-01-05T14:30:00Z",
            &[1.0],
            "2026-01-06T09:00:00Z",
        );
        append(
            &store,
            "A",
            "2026-01-07T14:30:00Z",
            &[2.0],
            "2026-01-08T09:00:00Z",
        );
        let r = run(
            &store,
            &params(vec![source(1, "A", BucketRule::Last), expr(2, "s1 * 2")]),
        );
        let p = &r.slots[0].provenance;
        assert_eq!(
            p.loaded,
            Some((ts("2026-01-05T14:30:00Z"), ts("2026-01-08T14:30:00Z")))
        );
        assert_eq!(p.latest_received_at, Some(ts("2026-01-08T09:00:00Z")));
        assert_eq!(p.health, None, "the service fills health");
        let e = &r.slots[1].provenance;
        assert_eq!(
            (e.loaded, e.latest_received_at, e.health.clone()),
            (None, None, None)
        );
        let r = run(&store, &params(vec![source(1, "NEVER", BucketRule::Last)]));
        assert!(r.buckets.is_empty());
        assert_eq!(
            r.slots[0].provenance.loaded, None,
            "an unfetched pair has no hull"
        );
    }
}

#[cfg(test)]
mod consistency_tests {
    use super::*;

    #[test]
    fn points_stats_and_coverage_share_one_snapshot() {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch("create table t as select 1::bigint i, 1.0::double v")
            .unwrap();
        let writer = conn.try_clone().unwrap();
        let statement = |sql: &str| Statement {
            sql: sql.into(),
            params: vec![],
        };
        let plan = SeriesPlan {
            slots: vec![1],
            points: statement("select i, v from t"),
            fractions: vec![0.5],
            percentiles: vec![(1, statement("select quantile_cont(v, 0.5) from t"))],
            bin_count: 1,
            bins: vec![(1, statement("select v, v+1, 1::bigint, 1::bigint from t"))],
            coverage: vec![(1, statement("select i, i, i from t"))],
        };
        let first = run_series_with(&conn, &plan, || {
            writer.execute_batch("update t set v = 2, i = 2").unwrap();
        })
        .unwrap();
        assert_eq!(first.slots[0].values[0], 1.);
        assert_eq!(first.slots[0].percentiles[0].1, 1.);
        assert_eq!(first.slots[0].bins, vec![(1., 2., 1)]);
        assert_eq!(
            first.slots[0].provenance.latest_received_at,
            Some(from_micros(1))
        );
        let next = run_series(&conn, &plan).unwrap();
        assert_eq!(next.slots[0].values[0], 2.);
        assert_eq!(next.slots[0].percentiles[0].1, 2.);
        assert_eq!(next.slots[0].bins, vec![(2., 3., 1)]);
        assert_ne!(first.slots[0].provenance, next.slots[0].provenance);
        // Failed statements must release the transaction before this worker's next request.
        let mut bad = plan.clone();
        bad.percentiles[0].1.sql = "select missing from t".into();
        assert!(run_series(&conn, &bad).is_err());
        assert!(run_series(&conn, &plan).is_ok());
    }
}
