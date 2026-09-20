//! The series query compiler (timeseries spec §6.2): one plan per
//! request — a points statement over every slot, a percentile and a
//! bins statement per slot, a coverage statement per source slot — all
//! pure string-in string-out with every value bound. The rows it reads
//! are Part 1's: live is `arg_max(value, received_at)` per `ts`, as-of
//! is `received_at <= t` (and `ts <= t`), there is no generation.
//!
//! Every stats statement carries the same CTE prefix as the points
//! statement and so re-runs the bucketing; `docs/perf.md` records what
//! that costs at a million rows and where a single grouping-sets
//! statement would take it if it ever matters.

use crate::store::StoreError;
use crate::store::series::{coverage_table, micros, series_table};
use duckdb::types::Value;
use geode_core::query::AsOf;
use geode_core::schema::SchemaSpec;
use geode_core::series::expr::{Ast, Expr, Op, expression_order};
use geode_core::series::{BucketRule, MAX_BINS, MIN_BINS, SeriesParams, SlotKind};

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

/// One expression node as SQL. Every binary node is wrapped in exactly
/// one pair of parentheses, so the tree's shape — not SQL's precedence —
/// decides what binds to what; a division carries its own zero guard
/// inside that pair, because a NULL is the only honest answer for a
/// bucket whose denominator vanished (spec §7).
fn lower(e: &Expr) -> Result<String, StoreError> {
    Ok(match e {
        Ast::Ref(n) => format!("s{n}.v"),
        Ast::Num(x) => {
            if !x.is_finite() {
                return Err(refuse(format!("literal {x} is not a finite number")));
            }
            format!("{x:?}")
        }
        Ast::Neg(inner) => format!("(-({}))", lower(inner)?),
        Ast::Bin(op, l, r) => {
            let (l, r) = (lower(l)?, lower(r)?);
            let inner = match op {
                Op::Add => format!("({l}) + ({r})"),
                Op::Sub => format!("({l}) - ({r})"),
                Op::Mul => format!("({l}) * ({r})"),
                Op::Div => format!("(case when ({r}) = 0 then null else ({l}) / ({r}) end)"),
            };
            format!("({inner})")
        }
    })
}

/// Every refusal the request can earn, in one place and in one order, so
/// the message a trader reads names the first thing wrong rather than
/// whichever check happened to run first. Answers the order the compiler
/// emits its expression CTEs in.
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
        }
    }
    expression_order(&params.series).map_err(|slot| refuse(format!("slot {slot} is on a cycle")))
}

/// The CTE prefix every statement shares, and its bound params in order.
///
/// Source CTEs come first, in request order, each collapsing the
/// bitemporal rows to one value per `ts` before bucketing — the inner
/// `arg_max(value, received_at)` is what makes a corrected point replace
/// its predecessor rather than join it. Expression CTEs follow in
/// `expression_order`, so every operand a lowering names already exists.
fn ctes(params: &SeriesParams, order: &[u8]) -> Result<(String, Vec<Value>), StoreError> {
    let table = series_table(&params.dataset);
    let interval = params.frequency.interval_sql();
    let mut parts: Vec<String> = Vec::new();
    let mut bound: Vec<Value> = Vec::new();
    let as_of = match &params.as_of {
        AsOf::Live => String::new(),
        AsOf::At(_) => {
            " and received_at <= make_timestamp(?) and ts <= make_timestamp(?)".to_string()
        }
    };
    for s in &params.series {
        if let SlotKind::Source {
            source,
            identity,
            rule,
        } = &s.kind
        {
            parts.push(format!(
                "s{n} as (\n  select time_bucket({interval}, ts) as b, {agg} as v\n  from (\n    select ts, arg_max(value, received_at) as v\n    from {table}\n    where source = ? and series_id = ? and ts >= make_timestamp(?) and ts < make_timestamp(?){as_of}\n    group by ts\n  )\n  group by b\n)",
                n = s.slot,
                agg = aggregate(*rule),
            ));
            bound.push(Value::Text(source.clone()));
            bound.push(Value::Text(identity.clone()));
            bound.push(Value::BigInt(micros(params.range.0)));
            bound.push(Value::BigInt(micros(params.range.1)));
            if let AsOf::At(t) = &params.as_of {
                bound.push(Value::BigInt(micros(*t)));
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
        let deps = e.slots();
        let anchor = deps[0];
        let joins: String = deps[1..]
            .iter()
            .map(|d| format!(" join s{d} on s{d}.b = s{anchor}.b"))
            .collect();
        parts.push(format!(
            "s{n} as (\n  select s{anchor}.b as b, {v} as v\n  from s{anchor}{joins}\n)",
            n = slot,
            v = lower(e)?,
        ));
    }
    Ok((format!("with {}", parts.join(",\n")), bound))
}

/// Compile one request into the statements that answer it. Pure: no
/// connection, no clock, nothing mutated — Task 4's `run_series` is what
/// puts these on a connection.
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
    // guarantees at least one source slot: an expression whose operands
    // are all expressions either sits on a cycle or bottoms out in a
    // reference-free expression, and both are refused above.)
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

    pub(super) fn expr(slot: u8, text: &str) -> SeriesSpec {
        let e = parse(text)
            .unwrap()
            .resolve(&mut |r: &RefName| match r {
                RefName::Handle(n) => Some(*n),
                _ => None,
            })
            .unwrap();
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
                "and ts < make_timestamp(?) and received_at <= make_timestamp(?) and ts <= make_timestamp(?)"
            ),
            "{}",
            plan.points.sql
        );
        assert_eq!(plan.points.params.len(), 6);
        assert_eq!(plan.points.params[4], micros("2026-01-08T12:00:00Z"));
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
                "s3 as (\n  select s1.b as b, ((case when (s2.v) = 0 then null else (s1.v) / (s2.v) end)) as v\n  from s1 join s2 on s2.b = s1.b\n)"
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
    fn expressions_are_emitted_operands_first_whatever_the_request_order() {
        let plan = compile_series(
            &schema(),
            &params(vec![
                expr(4, "s3 - s1"),
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
        assert!(s1 < s3 && s2 < s3 && s3 < s4, "{sql}");
        assert!(
            sql.contains("s3 as (\n  select s1.b as b, ((s1.v) * (2.0)) as v\n  from s1\n)"),
            "{sql}"
        );
        assert!(
            sql.contains(
                "s4 as (\n  select s1.b as b, ((s3.v) - (s1.v)) as v\n  from s1 join s3 on s3.b = s1.b\n)"
            ),
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
                .contains("(((-(((s1.v) + (1.5))))) * (s1.v)) as v"),
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
            expr(2, "s3 + s1"),
            expr(3, "s2"),
        ]));
        assert!(e.contains("cycle"), "{e}");
    }
}
