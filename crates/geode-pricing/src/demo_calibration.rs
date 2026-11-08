//! The demo forward calibrator. Deterministic, smooth and NOT a model: it
//! stands in for the desk's calibration engine behind the same
//! `Calibrator` trait so the calibration tile has stages that run, fits
//! that move between runs, a check that can hold the publish, and rows to
//! publish. Only the payload shapes are the contract; the real engine
//! replaces every formula below.
//!
//! Demo arithmetic, stage by stage:
//!
//! - `parity_fit`: the demo chain has one quote per strike with no
//!   call/put split, so no parity regression is possible. Each expiry's
//!   forward is the chain document's `forward` attribute moved by a
//!   residual of up to [`FORWARD_RESIDUAL_SCALE`] mean bid-ask widths
//!   (the widths of the strikes whose moneyness `strike / spot_ref` lies
//!   inside `strike_window`; the residual's sign and size come from a hash
//!   of underlying, stage, run and expiry), with `lo..hi` one mean width
//!   wide. `df` is a placeholder 1.0 with `lo == hi`: the stage sees only
//!   the chain and no curve. Tenors are 365.25-day year fractions from
//!   `quote_time`'s date to the expiry. An expiry with fewer than
//!   `min_strikes` strikes inside the window, or one on or before the quote
//!   date, is dropped and named in the summary.
//! - `implied_rates`: with `S` the chain's `spot_ref` and `PV` the sum of
//!   dividend amounts whose `ex_date` falls inside the tenor (undiscounted),
//!   `repo(T) = r_f(T) − ln(F / (S − PV)) / T`, `r_f` the `rates` source's
//!   series read piecewise-linearly in tenor and flat outside, and
//!   `box(T) = (r_f(T) + repo(T)) / 2 + BOX_BASIS`. Spot and the quote date
//!   come with the chain, as the builtin document says. The `lo..hi` half
//!   width of `repo` is the forward's half width over the forward, scaled
//!   by [`RATE_SPREAD_SCALE`]; `box`'s is half that.
//! - `check monotone_df`: a violation wherever `df` rises with tenor. So
//!   the held publish is reachable on demo data, runs
//!   [`FORCED_VIOLATION_RUNS`] of every [`FORCED_VIOLATION_CYCLE`] for an
//!   underlying also report one violation at the second tenor.
//! - `kalman_fit`: a linear Kalman update of the knot values (`kalman`),
//!   `process_noise` added to every knot's variance per run, measurement
//!   variance `((hi − lo) / 2)²` for `spread` (floored at
//!   [`MIN_MEASUREMENT_VARIANCE`]) or the fixed number. The first run for a
//!   (pipeline, underlying, stage) starts the mean by regularised least
//!   squares with [`INITIAL_VARIANCE`] on every knot. The fit is sampled
//!   on the `monthly_to_5y` grid.
//! - `rasterize`: the fit read on the grid; `publish`: held when a gate
//!   reports violations.
//!
//! State lives in memory behind one mutex and is lost on restart: the
//! filters, the last payload of every stage (what a run from a middle
//! stage, an evaluate and a publish read upstream of what they run) and a
//! per-(pipeline, underlying) run counter for the forced schedule. Nothing
//! is seeded from the clock; a given request sequence on a fresh engine
//! always answers the same numbers.

mod kalman;

use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use geode_core::calibration::{
    CalibrationError, CalibrationOutcome, CalibrationRequest, Calibrator, CheckResult, Fit,
    FitStats, Noise, Pipeline, Ports, PublishSource, Raster, RunAction, Series, Stage, StageInput,
    StageKind, StageName, StagePayload, StageResult, StageStatus, TermPoints,
};
use geode_core::document::{Column, DocumentRows, Value};
use kalman::Filter;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The name the calibrator registry selects this engine by.
pub const DEMO_CALIBRATOR: &str = "demo";

/// The one grid this engine rasterizes onto: 60 monthly tenors to five years.
pub const GRID_MONTHLY_TO_5Y: &str = "monthly_to_5y";
const GRID_MONTHS: usize = 60;

const DAYS_PER_YEAR: f64 = 365.25;

/// The parity residual: how many mean bid-ask widths the demo forward may
/// sit from the chain's `forward` attribute, either side.
const FORWARD_RESIDUAL_SCALE: f64 = 0.5;
/// Rate half width per unit of relative forward half width.
const RATE_SPREAD_SCALE: f64 = 4.0;
/// What the demo box rate sits above the mean of funding and repo.
const BOX_BASIS: f64 = 0.0015;
/// The knot variance a fresh or seeded filter starts with.
const INITIAL_VARIANCE: f64 = 1.0e-4;
/// A quoted width of zero must not make a measurement exact.
const MIN_MEASUREMENT_VARIANCE: f64 = 1.0e-12;

/// The forced `monotone_df` violation: runs `FORCED_VIOLATION_RUNS` (1-based)
/// of every `FORCED_VIOLATION_CYCLE` for an underlying hold the publish.
const FORCED_VIOLATION_CYCLE: u32 = 14;
const FORCED_VIOLATION_RUNS: [u32; 2] = [7, 8];

/// The only check kind the document admits.
const MONOTONE_DF: &str = "monotone_df";

/// (pipeline, underlying, stage).
type FilterKey = (String, String, StageName);

#[derive(Default)]
struct State {
    filters: HashMap<FilterKey, Filter>,
    /// The payload each stage last produced, read by stages a later run
    /// starts downstream of, by evaluate, and by publish.
    payloads: HashMap<FilterKey, StagePayload>,
    runs: HashMap<(String, String), RunRecord>,
}

#[derive(Default)]
struct RunRecord {
    /// Runs and seeds so far; the forced schedule counts these.
    count: u32,
    /// The reason the `publish` stage was held the last time it ran.
    publish_held: Option<String>,
}

pub struct DemoForwardCalibrator {
    state: Mutex<State>,
}

impl Default for DemoForwardCalibrator {
    fn default() -> Self {
        Self::new()
    }
}

impl DemoForwardCalibrator {
    pub fn new() -> Self {
        DemoForwardCalibrator {
            state: Mutex::new(State::default()),
        }
    }
}

/// What every stage of one run reads besides its inputs.
struct RunContext<'a> {
    pipeline: &'a Pipeline,
    underlying: &'a str,
    inputs: &'a [StageInput],
    /// 1-based count of runs for this pipeline and underlying.
    run_index: u32,
    chain: Option<ChainReference>,
}

/// Spot and the quote date, which come with the chain rather than through
/// a port.
#[derive(Clone, Copy)]
struct ChainReference {
    spot: f64,
    as_of: NaiveDate,
}

impl RunContext<'_> {
    fn docs(&self, stage: &str) -> &[Arc<DocumentRows>] {
        self.inputs
            .iter()
            .find(|i| i.stage == stage)
            .map_or(&[], |i| i.rows.as_slice())
    }

    fn key(&self, stage: &str) -> FilterKey {
        (
            self.pipeline.name.clone(),
            self.underlying.to_string(),
            stage.to_string(),
        )
    }
}

/// A stage that ran to an end: Ok or Held with its summary and payload. A
/// `Failed` is the `Err` string.
struct Done {
    status: StageStatus,
    summary: String,
    payload: StagePayload,
}

fn ok(summary: String, payload: StagePayload) -> Result<Done, String> {
    Ok(Done {
        status: StageStatus::Ok,
        summary,
        payload,
    })
}

impl Calibrator for DemoForwardCalibrator {
    fn name(&self) -> &str {
        DEMO_CALIBRATOR
    }

    fn run(&self, request: &CalibrationRequest) -> Result<CalibrationOutcome, CalibrationError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let pipeline = &request.pipeline;
        let selected: HashSet<String> = match &request.action {
            RunAction::Publish { from } => return publish(request, &mut state, from),
            RunAction::Evaluate { stage, params } => {
                return evaluate(request, &mut state, stage, params);
            }
            RunAction::Run { from: None } => {
                pipeline.stages.iter().map(|s| s.name.clone()).collect()
            }
            RunAction::Run { from: Some(from) } => {
                require_stage(pipeline, from)?;
                downstream(pipeline, from)
            }
            RunAction::Seed { stage, params } => {
                let (stage_ref, knots) = kalman_stage(pipeline, stage)?;
                if params.len() != knots.len() {
                    return Err(CalibrationError(format!(
                        "{stage} has {} knots, got {} params",
                        knots.len(),
                        params.len()
                    )));
                }
                let key = (
                    pipeline.name.clone(),
                    request.underlying.clone(),
                    stage_ref.name.clone(),
                );
                state
                    .filters
                    .insert(key, Filter::new(params.clone(), INITIAL_VARIANCE));
                downstream(pipeline, stage)
            }
        };
        let State {
            filters,
            payloads,
            runs,
        } = &mut *state;
        let record = runs
            .entry((pipeline.name.clone(), request.underlying.clone()))
            .or_default();
        record.count += 1;
        let ctx = RunContext {
            pipeline,
            underlying: &request.underlying,
            inputs: &request.inputs,
            run_index: record.count,
            chain: chain_reference(pipeline, &request.inputs),
        };
        let mut results: Vec<StageResult> = Vec::with_capacity(pipeline.stages.len());
        for stage in pipeline.topological() {
            if !selected.contains(&stage.name) {
                results.push(skipped(stage));
                continue;
            }
            let mut inputs: Vec<&StagePayload> = Vec::with_capacity(stage.inputs.len());
            let mut blocked = false;
            // An upstream stage this run skipped or failed skips this one
            // (the transitive rule) before a missing cached input can fail
            // it: a stage that would not have run has nothing to fail on.
            let mut missing: Option<String> = None;
            for input in &stage.inputs {
                let Some(upstream) = pipeline.stage(input) else {
                    missing.get_or_insert_with(|| {
                        format!("input {input} is not a stage of {}", pipeline.name)
                    });
                    continue;
                };
                if matches!(upstream.kind, StageKind::Source { .. }) {
                    inputs.push(&StagePayload::None);
                    continue;
                }
                if selected.contains(input)
                    && let Some(result) = results.iter().find(|r| r.name == *input)
                {
                    if result.status == StageStatus::Ok {
                        inputs.push(&result.payload);
                    } else {
                        blocked = true;
                    }
                    continue;
                }
                match payloads.get(&ctx.key(input)) {
                    Some(payload) => inputs.push(payload),
                    None => {
                        missing.get_or_insert_with(|| format!("input {input} has not run"));
                    }
                }
            }
            let result = if blocked {
                skipped(stage)
            } else if let Some(reason) = missing {
                failed(stage, reason)
            } else {
                self.execute(stage, &inputs, &ctx, filters)
            };
            results.push(result);
        }
        for result in &results {
            if result.status == StageStatus::Ok && selected.contains(&result.name) {
                payloads.insert(ctx.key(&result.name), result.payload.clone());
            }
        }
        if let Some(publish) = results.iter().find(|r| {
            matches!(
                pipeline.stage(&r.name).map(|s| &s.kind),
                Some(StageKind::Publish { .. })
            )
        }) {
            match &publish.status {
                StageStatus::Ok => record.publish_held = None,
                StageStatus::Held(reason) => record.publish_held = Some(reason.clone()),
                _ => {}
            }
        }
        Ok(CalibrationOutcome {
            key: request.key,
            tag: request.tag,
            submitted: request.submitted,
            action: request.action.clone(),
            stages: results,
            publish_rows: None,
        })
    }
}

impl DemoForwardCalibrator {
    /// Runs one stage over its typed inputs. Sources read the request's
    /// documents through `ctx`; every other kind reads the payloads of
    /// its inputs, checked here so a wrong payload fails the stage by name
    /// and never panics. `filters` is the mutable half of the state a
    /// `kalman_fit` keeps between runs.
    fn execute(
        &self,
        stage: &Stage,
        inputs: &[&StagePayload],
        ctx: &RunContext,
        filters: &mut HashMap<FilterKey, Filter>,
    ) -> StageResult {
        let started = Instant::now();
        let done = match stage.kind.ports() {
            Ports::Fixed(ports) if ports.len() != inputs.len() => Err(format!(
                "{} takes {} inputs, got {}",
                stage.kind.kind_name(),
                ports.len(),
                inputs.len()
            )),
            _ => match &stage.kind {
                StageKind::Source { .. } => source(stage, ctx),
                StageKind::ParityFit {
                    min_strikes,
                    strike_window,
                } => parity_fit(stage, ctx, *min_strikes, *strike_window),
                StageKind::ImpliedRates => implied_rates(stage, inputs, ctx),
                StageKind::Check { check } => check_stage(stage, inputs, ctx, check),
                StageKind::KalmanFit {
                    series,
                    knots,
                    process_noise,
                    measurement_noise,
                } => kalman_fit(
                    stage,
                    inputs,
                    ctx,
                    filters,
                    series,
                    knots,
                    *process_noise,
                    measurement_noise,
                ),
                StageKind::Rasterize { grid } => rasterize(stage, inputs, grid),
                StageKind::Publish { target, .. } => publish_stage(stage, inputs, target),
            },
        };
        let (status, summary, payload) = match done {
            Ok(d) => (d.status, d.summary, d.payload),
            Err(reason) => (
                StageStatus::Failed(reason),
                String::new(),
                StagePayload::None,
            ),
        };
        StageResult {
            name: stage.name.clone(),
            status,
            ran_at: Some(Utc::now()),
            elapsed: started.elapsed(),
            summary,
            payload,
        }
    }
}

fn skipped(stage: &Stage) -> StageResult {
    StageResult {
        name: stage.name.clone(),
        status: StageStatus::Skipped,
        ran_at: None,
        elapsed: Duration::ZERO,
        summary: "not run".to_string(),
        payload: StagePayload::None,
    }
}

fn failed(stage: &Stage, reason: String) -> StageResult {
    StageResult {
        name: stage.name.clone(),
        status: StageStatus::Failed(reason),
        ran_at: Some(Utc::now()),
        elapsed: Duration::ZERO,
        summary: String::new(),
        payload: StagePayload::None,
    }
}

fn require_stage<'a>(pipeline: &'a Pipeline, name: &str) -> Result<&'a Stage, CalibrationError> {
    pipeline
        .stage(name)
        .ok_or_else(|| CalibrationError(format!("no stage {name} in {}", pipeline.name)))
}

/// The `kalman_fit` stage `name` and its knots; any other kind is refused.
fn kalman_stage<'a>(
    pipeline: &'a Pipeline,
    name: &str,
) -> Result<(&'a Stage, &'a [f64]), CalibrationError> {
    let stage = require_stage(pipeline, name)?;
    match &stage.kind {
        StageKind::KalmanFit { knots, .. } => Ok((stage, knots)),
        other => Err(CalibrationError(format!(
            "{name} is a {} stage, not kalman_fit",
            other.kind_name()
        ))),
    }
}

/// `from` and every stage that reads it, directly or through others.
fn downstream(pipeline: &Pipeline, from: &str) -> HashSet<String> {
    let mut set: HashSet<String> = HashSet::new();
    let mut queue = vec![from.to_string()];
    while let Some(name) = queue.pop() {
        if !set.insert(name.clone()) {
            continue;
        }
        for consumer in pipeline.consumers(&name) {
            queue.push(consumer.name.clone());
        }
    }
    set
}

/// Spot and the quote date from the first document of the chain source.
fn chain_reference(pipeline: &Pipeline, inputs: &[StageInput]) -> Option<ChainReference> {
    let chain = pipeline.stages.iter().find(
        |s| matches!(&s.kind, StageKind::Source { dataset, .. } if dataset == "option_chain"),
    )?;
    let doc = inputs
        .iter()
        .find(|i| i.stage == chain.name)?
        .rows
        .first()?;
    let spot = f64_attribute(doc, "spot_ref").ok()?;
    let as_of = quote_date(doc).ok()?;
    Some(ChainReference { spot, as_of })
}

// ---- payload typing at the boundary ----

fn payload_name(payload: &StagePayload) -> &'static str {
    match payload {
        StagePayload::None => "nothing",
        StagePayload::TermPoints(_) => "term points",
        StagePayload::Fit(_) => "a fit",
        StagePayload::Raster(_) => "a raster",
        StagePayload::Check(_) => "a check",
        StagePayload::PublishReady => "publish-ready",
    }
}

fn wrong(stage: &Stage, port: usize, got: &StagePayload, want: &str) -> String {
    format!(
        "input {} is {}, not {want}",
        stage.inputs.get(port).map_or("?", String::as_str),
        payload_name(got)
    )
}

fn term_points<'a>(
    stage: &Stage,
    port: usize,
    inputs: &[&'a StagePayload],
) -> Result<&'a TermPoints, String> {
    match inputs.get(port) {
        Some(StagePayload::TermPoints(p)) => Ok(p),
        Some(other) => Err(wrong(stage, port, other, "term points")),
        None => Err(format!("input {port} is missing")),
    }
}

fn fit_payload<'a>(
    stage: &Stage,
    port: usize,
    inputs: &[&'a StagePayload],
) -> Result<&'a Fit, String> {
    match inputs.get(port) {
        Some(StagePayload::Fit(f)) => Ok(f),
        Some(other) => Err(wrong(stage, port, other, "a fit")),
        None => Err(format!("input {port} is missing")),
    }
}

fn series<'a>(points: &'a TermPoints, name: &str) -> Result<&'a Series, String> {
    points
        .series
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| format!("points have no {name} series"))
}

// ---- document columns ----

fn f64_column<'a>(cols: &'a [(String, Column)], name: &str) -> Result<&'a [f64], String> {
    match cols.iter().find(|(n, _)| n == name) {
        Some((_, Column::F64(v))) => Ok(v),
        Some(_) => Err(format!("column {name} is not numeric")),
        None => Err(format!("document lacks the column {name}")),
    }
}

fn date_column<'a>(cols: &'a [(String, Column)], name: &str) -> Result<&'a [NaiveDate], String> {
    match cols.iter().find(|(n, _)| n == name) {
        Some((_, Column::Date(v))) => Ok(v),
        Some(_) => Err(format!("column {name} is not a date")),
        None => Err(format!("document lacks the column {name}")),
    }
}

fn f64_attribute(doc: &DocumentRows, name: &str) -> Result<f64, String> {
    match doc.attributes.iter().find(|(n, _)| n == name) {
        Some((_, Value::F64(v))) if v.is_finite() => Ok(*v),
        Some(_) => Err(format!("attribute {name} is not a finite number")),
        None => Err(format!("document lacks the attribute {name}")),
    }
}

/// The date of the chain's `quote_time` (RFC 3339).
fn quote_date(doc: &DocumentRows) -> Result<NaiveDate, String> {
    match doc.attributes.iter().find(|(n, _)| n == "quote_time") {
        Some((_, Value::Utf8(text))) => DateTime::parse_from_rfc3339(text)
            .map(|t| t.with_timezone(&Utc).date_naive())
            .map_err(|e| format!("quote_time {text} is not RFC 3339: {e}")),
        Some(_) => Err("attribute quote_time is not text".to_string()),
        None => Err("document lacks the attribute quote_time".to_string()),
    }
}

fn year_fraction(from: NaiveDate, to: NaiveDate) -> f64 {
    (to - from).num_days() as f64 / DAYS_PER_YEAR
}

/// A number in `[0, 1)` from the text parts and the run: FNV-1a, so the
/// same inputs answer the same number on every build and machine.
fn unit(parts: &[&str], run: u32) -> f64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |b: u8| {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    };
    for part in parts {
        part.bytes().for_each(&mut mix);
        mix(0xff);
    }
    run.to_le_bytes().into_iter().for_each(&mut mix);
    (h >> 11) as f64 / (1u64 << 53) as f64
}

fn grid_tenors(grid: &str) -> Option<Vec<f64>> {
    (grid == GRID_MONTHLY_TO_5Y).then(|| (1..=GRID_MONTHS).map(|m| m as f64 / 12.0).collect())
}

fn pct(v: f64) -> f64 {
    v * 100.0
}

fn range(values: &[f64]) -> (f64, f64) {
    values
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
            (lo.min(*v), hi.max(*v))
        })
}

// ---- the stages ----

fn source(stage: &Stage, ctx: &RunContext) -> Result<Done, String> {
    let n = ctx.docs(&stage.name).len();
    let noun = if n == 1 { "document" } else { "documents" };
    ok(format!("{n} {noun}"), StagePayload::None)
}

fn parity_fit(
    stage: &Stage,
    ctx: &RunContext,
    min_strikes: usize,
    (lo, hi): (f64, f64),
) -> Result<Done, String> {
    let chain = ctx.docs(&stage.inputs[0]);
    if chain.is_empty() {
        return Err(format!("{} has no documents", stage.inputs[0]));
    }
    let mut points: Vec<(f64, f64, f64)> = Vec::with_capacity(chain.len());
    let mut dropped: Vec<String> = Vec::new();
    for doc in chain {
        let expiry_text = doc
            .key
            .get(1)
            .ok_or_else(|| "chain document key has no expiry".to_string())?;
        let expiry = NaiveDate::parse_from_str(expiry_text, "%Y-%m-%d")
            .map_err(|_| format!("chain expiry {expiry_text} is not a date"))?;
        let forward = f64_attribute(doc, "forward")?;
        let spot = f64_attribute(doc, "spot_ref")?;
        let as_of = quote_date(doc)?;
        let strikes = f64_column(&doc.axes, "strike")?;
        let bid = f64_column(&doc.values, "bid")?;
        let ask = f64_column(&doc.values, "ask")?;
        let t = year_fraction(as_of, expiry);
        if t <= 0.0 {
            dropped.push(format!("{expiry_text} skipped: expired"));
            continue;
        }
        let inside: Vec<usize> = (0..strikes.len().min(bid.len()).min(ask.len()))
            .filter(|&i| {
                let m = strikes[i] / spot;
                m >= lo && m <= hi
            })
            .collect();
        if inside.len() < min_strikes {
            dropped.push(format!(
                "{expiry_text} skipped: {} strike{}",
                inside.len(),
                if inside.len() == 1 { "" } else { "s" }
            ));
            continue;
        }
        let mean_width = inside
            .iter()
            .map(|&i| (ask[i] - bid[i]).max(0.0))
            .sum::<f64>()
            / inside.len() as f64;
        let u = unit(&[ctx.underlying, &stage.name, expiry_text], ctx.run_index) - 0.5;
        let mid = forward + FORWARD_RESIDUAL_SCALE * u * mean_width;
        points.push((t, mid, mean_width / 2.0));
    }
    if points.is_empty() {
        return Err(format!(
            "no expiry has {min_strikes} strikes inside {lo:.2}–{hi:.2} · {}",
            dropped.join(" · ")
        ));
    }
    points.sort_by(|a, b| a.0.total_cmp(&b.0));
    let n = points.len();
    let term = TermPoints {
        tenors: points.iter().map(|p| p.0).collect(),
        series: vec![
            Series {
                name: "forward".to_string(),
                mid: points.iter().map(|p| p.1).collect(),
                lo: points.iter().map(|p| p.1 - p.2).collect(),
                hi: points.iter().map(|p| p.1 + p.2).collect(),
            },
            Series {
                name: "df".to_string(),
                mid: vec![1.0; n],
                lo: vec![1.0; n],
                hi: vec![1.0; n],
            },
        ],
    };
    let mut summary = format!("{n} of {} expiries", chain.len());
    for d in &dropped {
        summary.push_str(" · ");
        summary.push_str(d);
    }
    ok(summary, StagePayload::TermPoints(term))
}

/// The `(tenor, value)` points of the series a curve source reads, sorted
/// by tenor.
fn funding_curve(ctx: &RunContext, source_name: &str) -> Result<(Vec<f64>, Vec<f64>), String> {
    let wanted = match ctx.pipeline.stage(source_name).map(|s| &s.kind) {
        Some(StageKind::Source { series, .. }) => series.clone(),
        _ => None,
    };
    let docs = ctx.docs(source_name);
    let doc = match &wanted {
        Some(series) => docs.iter().find(|d| d.key.get(1) == Some(series)),
        None => docs.first(),
    }
    .ok_or_else(|| match &wanted {
        Some(series) => format!("{source_name} has no {series} curve"),
        None => format!("{source_name} has no documents"),
    })?;
    let tenors = f64_column(&doc.axes, "tenor")?;
    let values = f64_column(&doc.values, "value")?;
    let mut pairs: Vec<(f64, f64)> = tenors.iter().copied().zip(values.iter().copied()).collect();
    pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
    if pairs.is_empty() {
        return Err(format!("{source_name} curve has no points"));
    }
    Ok(pairs.into_iter().unzip())
}

/// Dividend `(ex tenor, amount)` pairs from every schedule document.
fn dividend_tenors(
    ctx: &RunContext,
    source_name: &str,
    as_of: NaiveDate,
) -> Result<Vec<(f64, f64)>, String> {
    let mut out = Vec::new();
    for doc in ctx.docs(source_name) {
        let ex = date_column(&doc.values, "ex_date")?;
        let amount = f64_column(&doc.values, "amount")?;
        for (d, a) in ex.iter().zip(amount) {
            out.push((year_fraction(as_of, *d), *a));
        }
    }
    Ok(out)
}

fn implied_rates(
    stage: &Stage,
    inputs: &[&StagePayload],
    ctx: &RunContext,
) -> Result<Done, String> {
    let parity = term_points(stage, 0, inputs)?;
    let forward = series(parity, "forward")?;
    let chain = ctx
        .chain
        .ok_or_else(|| "the chain carries no spot_ref or quote_time".to_string())?;
    let dividends = dividend_tenors(ctx, &stage.inputs[1], chain.as_of)?;
    let (curve_tenors, curve_values) = funding_curve(ctx, &stage.inputs[2])?;
    let n = parity.tenors.len();
    let mut repo = Series {
        name: "repo".to_string(),
        mid: Vec::with_capacity(n),
        lo: Vec::with_capacity(n),
        hi: Vec::with_capacity(n),
    };
    let mut bx = Series {
        name: "box".to_string(),
        mid: Vec::with_capacity(n),
        lo: Vec::with_capacity(n),
        hi: Vec::with_capacity(n),
    };
    for (i, &t) in parity.tenors.iter().enumerate() {
        let f = forward.mid[i];
        let half_f = (forward.hi[i] - forward.lo[i]) / 2.0;
        let rf = kalman::interpolate(&curve_tenors, &curve_values, t);
        let pv: f64 = dividends
            .iter()
            .filter(|(ex, _)| *ex > 0.0 && *ex <= t)
            .map(|(_, a)| a)
            .sum();
        let base = chain.spot - pv;
        if base <= 0.0 || f <= 0.0 || t <= 0.0 || !(base.is_finite() && f.is_finite()) {
            return Err(format!(
                "tenor {t:.2}y: forward {f:.2}, spot less dividends {base:.2}"
            ));
        }
        let r = rf - (f / base).ln() / t;
        let half = RATE_SPREAD_SCALE * half_f / f;
        repo.mid.push(r);
        repo.lo.push(r - half);
        repo.hi.push(r + half);
        let b = (rf + r) / 2.0 + BOX_BASIS;
        bx.mid.push(b);
        bx.lo.push(b - half / 2.0);
        bx.hi.push(b + half / 2.0);
    }
    let (rlo, rhi) = range(&repo.mid);
    let (blo, bhi) = range(&bx.mid);
    let summary = format!(
        "{n} tenors · repo {:.2}–{:.2}% · box {:.2}–{:.2}%",
        pct(rlo),
        pct(rhi),
        pct(blo),
        pct(bhi)
    );
    ok(
        summary,
        StagePayload::TermPoints(TermPoints {
            tenors: parity.tenors.clone(),
            series: vec![repo, bx],
        }),
    )
}

fn forced_violation(run_index: u32) -> bool {
    let position = (run_index - 1) % FORCED_VIOLATION_CYCLE + 1;
    FORCED_VIOLATION_RUNS.contains(&position)
}

fn check_stage(
    stage: &Stage,
    inputs: &[&StagePayload],
    ctx: &RunContext,
    check: &str,
) -> Result<Done, String> {
    if check != MONOTONE_DF {
        return Err(format!("unknown check {check}"));
    }
    let points = term_points(stage, 0, inputs)?;
    let df = series(points, "df")?;
    let mut violations: Vec<String> = Vec::new();
    for i in 1..points.tenors.len().min(df.mid.len()) {
        if df.mid[i] > df.mid[i - 1] + 1e-12 {
            violations.push(format!(
                "df rises at {:.2}y ({:.4} → {:.4})",
                points.tenors[i],
                df.mid[i - 1],
                df.mid[i]
            ));
        }
    }
    if forced_violation(ctx.run_index)
        && let Some(t) = points.tenors.get(1)
    {
        violations.push(format!(
            "df rises at {t:.2}y (forced on run {} of {FORCED_VIOLATION_CYCLE})",
            (ctx.run_index - 1) % FORCED_VIOLATION_CYCLE + 1
        ));
    }
    let summary = match violations.as_slice() {
        [] => format!("passed · {} tenors", points.tenors.len()),
        [one] => format!("1 violation · {one}"),
        many => format!("{} violations · {}", many.len(), many[0]),
    };
    ok(summary, StagePayload::Check(CheckResult { violations }))
}

/// The fit of `params` on `knots` sampled on the grid, with its stats
/// against the points.
fn fit_from(
    series_name: &str,
    knots: &[f64],
    params: &[f64],
    tenors: &[f64],
    ys: &[f64],
    innovation: f64,
) -> Fit {
    let grid = grid_tenors(GRID_MONTHLY_TO_5Y).unwrap_or_default();
    let values: Vec<f64> = grid
        .iter()
        .map(|t| kalman::interpolate(knots, params, *t))
        .collect();
    let mut sq = 0.0;
    let mut max_residual = 0.0_f64;
    let mut max_residual_tenor = 0.0;
    for (t, y) in tenors.iter().zip(ys) {
        let r = y - kalman::interpolate(knots, params, *t);
        sq += r * r;
        if r.abs() > max_residual.abs() {
            max_residual = r;
            max_residual_tenor = *t;
        }
    }
    let rmse = if tenors.is_empty() {
        0.0
    } else {
        (sq / tenors.len() as f64).sqrt()
    };
    Fit {
        series: series_name.to_string(),
        params: knots
            .iter()
            .zip(params)
            .map(|(k, v)| (format!("{k:.2}"), *v))
            .collect(),
        tenors: grid,
        values,
        stats: FitStats {
            rmse,
            max_residual,
            max_residual_tenor,
            innovation,
        },
    }
}

/// The finite `(tenor, mid, variance)` measurements of `series` under
/// `noise`.
fn measurements(points: &TermPoints, s: &Series, noise: &Noise) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let mut tenors = Vec::new();
    let mut ys = Vec::new();
    let mut vars = Vec::new();
    for (i, t) in points.tenors.iter().enumerate() {
        let (Some(mid), Some(lo), Some(hi)) = (s.mid.get(i), s.lo.get(i), s.hi.get(i)) else {
            break;
        };
        if !t.is_finite() || !mid.is_finite() {
            continue;
        }
        let var = match noise {
            Noise::Spread => {
                let half = (hi - lo) / 2.0;
                if half.is_finite() {
                    (half * half).max(MIN_MEASUREMENT_VARIANCE)
                } else {
                    MIN_MEASUREMENT_VARIANCE
                }
            }
            Noise::Fixed(v) => *v,
        };
        tenors.push(*t);
        ys.push(*mid);
        vars.push(var);
    }
    (tenors, ys, vars)
}

fn bp(v: f64) -> f64 {
    v * 10_000.0
}

#[allow(clippy::too_many_arguments)]
fn kalman_fit(
    stage: &Stage,
    inputs: &[&StagePayload],
    ctx: &RunContext,
    filters: &mut HashMap<FilterKey, Filter>,
    series_name: &str,
    knots: &[f64],
    process_noise: f64,
    noise: &Noise,
) -> Result<Done, String> {
    let points = term_points(stage, 0, inputs)?;
    let s = series(points, series_name)?;
    let (tenors, ys, vars) = measurements(points, s, noise);
    if tenors.is_empty() {
        return Err(format!("no points for {series_name}"));
    }
    if knots.is_empty() {
        return Err("no knots".to_string());
    }
    // A filter this call starts (no state yet, or a knot count the state
    // no longer matches) is the least-squares fit itself; one that already
    // existed, seeded or run, takes the time step and the update.
    let mut started_here = false;
    let filter = filters.entry(ctx.key(&stage.name)).or_insert_with(|| {
        started_here = true;
        Filter::new(kalman::least_squares(knots, &tenors, &ys), INITIAL_VARIANCE)
    });
    if filter.mean.len() != knots.len() {
        *filter = Filter::new(kalman::least_squares(knots, &tenors, &ys), INITIAL_VARIANCE);
        started_here = true;
    }
    let innovation = if started_here {
        0.0
    } else {
        filter.predict(process_noise);
        filter.update(knots, &tenors, &ys, &vars)
    };
    let fit = fit_from(series_name, knots, &filter.mean, &tenors, &ys, innovation);
    let summary = format!(
        "{} knots · rmse {:.1}bp · innovation {:.1}bp",
        knots.len(),
        bp(fit.stats.rmse),
        bp(innovation)
    );
    ok(summary, StagePayload::Fit(fit))
}

fn raster_from(fit: &Fit, grid: &str) -> Result<Raster, String> {
    let tenors = grid_tenors(grid).ok_or_else(|| format!("unknown grid {grid}"))?;
    let values = tenors
        .iter()
        .map(|t| kalman::interpolate(&fit.tenors, &fit.values, *t))
        .collect();
    Ok(Raster {
        series: fit.series.clone(),
        tenors,
        values,
    })
}

fn rasterize(stage: &Stage, inputs: &[&StagePayload], grid: &str) -> Result<Done, String> {
    let fit = fit_payload(stage, 0, inputs)?;
    let raster = raster_from(fit, grid)?;
    let (lo, hi) = range(&raster.values);
    let summary = format!(
        "{} points · {:.2}–{:.2}%",
        raster.tenors.len(),
        pct(lo),
        pct(hi)
    );
    ok(summary, StagePayload::Raster(raster))
}

fn publish_stage(stage: &Stage, inputs: &[&StagePayload], target: &str) -> Result<Done, String> {
    let mut rasters = 0;
    let mut held: Vec<String> = Vec::new();
    for (port, payload) in inputs.iter().enumerate() {
        match payload {
            StagePayload::Raster(_) => rasters += 1,
            StagePayload::Check(c) if c.violations.is_empty() => {}
            StagePayload::Check(c) => held.push(format!(
                "{}: {} violation{}",
                stage.inputs.get(port).map_or("?", String::as_str),
                c.violations.len(),
                if c.violations.len() == 1 { "" } else { "s" }
            )),
            other => return Err(wrong(stage, port, other, "a raster or a check")),
        }
    }
    if rasters == 0 {
        return Err("publish has no raster to publish".to_string());
    }
    if held.is_empty() {
        ok(
            format!("{rasters} series → {target}"),
            StagePayload::PublishReady,
        )
    } else {
        Ok(Done {
            status: StageStatus::Held(held.join(" · ")),
            summary: format!("held by {}", held.join(" · ")),
            payload: StagePayload::None,
        })
    }
}

// ---- the user actions ----

/// `Evaluate`: the fit at `params`, against the points the stage last ran
/// over. Reads the state and writes nothing.
fn evaluate(
    request: &CalibrationRequest,
    state: &mut State,
    stage_name: &str,
    params: &[f64],
) -> Result<CalibrationOutcome, CalibrationError> {
    let pipeline = &request.pipeline;
    let (stage, knots) = kalman_stage(pipeline, stage_name)?;
    if params.len() != knots.len() {
        return Err(CalibrationError(format!(
            "{stage_name} has {} knots, got {} params",
            knots.len(),
            params.len()
        )));
    }
    let StageKind::KalmanFit {
        series: series_name,
        measurement_noise,
        ..
    } = &stage.kind
    else {
        unreachable!("kalman_stage answered a kalman_fit");
    };
    let key = |name: &str| {
        (
            pipeline.name.clone(),
            request.underlying.clone(),
            name.to_string(),
        )
    };
    let started = Instant::now();
    let result = match stage
        .inputs
        .first()
        .and_then(|input| state.payloads.get(&key(input)))
    {
        Some(StagePayload::TermPoints(points)) => match series(points, series_name) {
            Ok(s) => {
                let (tenors, ys, _) = measurements(points, s, measurement_noise);
                let fit = fit_from(series_name, knots, params, &tenors, &ys, 0.0);
                let summary = format!(
                    "{} knots · rmse {:.1}bp · draft",
                    knots.len(),
                    bp(fit.stats.rmse)
                );
                (StageStatus::Ok, summary, StagePayload::Fit(fit))
            }
            Err(reason) => (
                StageStatus::Failed(reason),
                String::new(),
                StagePayload::None,
            ),
        },
        Some(other) => (
            StageStatus::Failed(wrong(stage, 0, other, "term points")),
            String::new(),
            StagePayload::None,
        ),
        None => (
            StageStatus::Failed(format!(
                "input {} has not run",
                stage.inputs.first().map_or("?", String::as_str)
            )),
            String::new(),
            StagePayload::None,
        ),
    };
    let stages = pipeline
        .topological()
        .into_iter()
        .map(|s| {
            if s.name == stage.name {
                StageResult {
                    name: s.name.clone(),
                    status: result.0.clone(),
                    ran_at: Some(Utc::now()),
                    elapsed: started.elapsed(),
                    summary: result.1.clone(),
                    payload: result.2.clone(),
                }
            } else {
                skipped(s)
            }
        })
        .collect();
    Ok(CalibrationOutcome {
        key: request.key,
        tag: request.tag,
        submitted: request.submitted,
        action: request.action.clone(),
        stages,
        publish_rows: None,
    })
}

/// `Publish`: the rate-curve rows of every rasterize stage feeding the
/// publish stage, from the filters as they stand or with one stage's
/// draft parameters. Nothing runs; refused until a run has happened and
/// while the last run's publish stage is held.
fn publish(
    request: &CalibrationRequest,
    state: &mut State,
    from: &PublishSource,
) -> Result<CalibrationOutcome, CalibrationError> {
    let pipeline = &request.pipeline;
    let record = state
        .runs
        .get(&(pipeline.name.clone(), request.underlying.clone()))
        .filter(|r| r.count > 0)
        .ok_or_else(|| {
            CalibrationError(format!(
                "no run of {} has happened for {}",
                pipeline.name, request.underlying
            ))
        })?;
    if let Some(reason) = record.publish_held.as_ref() {
        return Err(CalibrationError(format!("publish is held: {reason}")));
    }
    let publish = pipeline
        .stages
        .iter()
        .find(|s| matches!(s.kind, StageKind::Publish { .. }))
        .ok_or_else(|| CalibrationError(format!("{} has no publish stage", pipeline.name)))?;
    let draft = match from {
        PublishSource::Filter => None,
        PublishSource::Draft { stage, params } => {
            let (_, knots) = kalman_stage(pipeline, stage)?;
            if params.len() != knots.len() {
                return Err(CalibrationError(format!(
                    "{stage} has {} knots, got {} params",
                    knots.len(),
                    params.len()
                )));
            }
            Some((stage.as_str(), params.as_slice()))
        }
    };
    let key = |name: &str| {
        (
            pipeline.name.clone(),
            request.underlying.clone(),
            name.to_string(),
        )
    };
    let published_time = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let started = Instant::now();
    let mut rasters: HashMap<String, Raster> = HashMap::new();
    let mut rows: Vec<DocumentRows> = Vec::new();
    for input in &publish.inputs {
        let Some(raster_stage) = pipeline.stage(input) else {
            continue;
        };
        let StageKind::Rasterize { grid } = &raster_stage.kind else {
            continue;
        };
        let fit_name = raster_stage
            .inputs
            .first()
            .ok_or_else(|| CalibrationError(format!("{} has no fit input", raster_stage.name)))?;
        let (fit_stage, knots) = kalman_stage(pipeline, fit_name)?;
        let StageKind::KalmanFit {
            series: series_name,
            ..
        } = &fit_stage.kind
        else {
            unreachable!("kalman_stage answered a kalman_fit");
        };
        let params: Vec<f64> = match draft {
            Some((stage, params)) if stage == fit_stage.name => params.to_vec(),
            _ => state
                .filters
                .get(&key(&fit_stage.name))
                .map(|f| f.mean.clone())
                .ok_or_else(|| CalibrationError(format!("{} has not run", fit_stage.name)))?,
        };
        let fit = fit_from(series_name, knots, &params, &[], &[], 0.0);
        let raster = raster_from(&fit, grid).map_err(CalibrationError)?;
        rows.push(DocumentRows {
            key: vec![request.underlying.clone(), series_name.clone()],
            attributes: vec![(
                "published_time".to_string(),
                Value::Utf8(published_time.clone()),
            )],
            axes: vec![("tenor".to_string(), Column::F64(raster.tenors.clone()))],
            values: vec![("value".to_string(), Column::F64(raster.values.clone()))],
        });
        rasters.insert(raster_stage.name.clone(), raster);
    }
    if rows.is_empty() {
        return Err(CalibrationError(format!(
            "{} publishes no rasterized series",
            publish.name
        )));
    }
    let StageKind::Publish { target, .. } = &publish.kind else {
        unreachable!("found by kind");
    };
    let stages = pipeline
        .topological()
        .into_iter()
        .map(|s| {
            if let Some(raster) = rasters.get(&s.name) {
                let (lo, hi) = range(&raster.values);
                StageResult {
                    name: s.name.clone(),
                    status: StageStatus::Ok,
                    ran_at: Some(Utc::now()),
                    elapsed: started.elapsed(),
                    summary: format!(
                        "{} points · {:.2}–{:.2}%",
                        raster.tenors.len(),
                        pct(lo),
                        pct(hi)
                    ),
                    payload: StagePayload::Raster(raster.clone()),
                }
            } else if s.name == publish.name {
                StageResult {
                    name: s.name.clone(),
                    status: StageStatus::Ok,
                    ran_at: Some(Utc::now()),
                    elapsed: started.elapsed(),
                    summary: format!("{} series → {target}", rows.len()),
                    payload: StagePayload::PublishReady,
                }
            } else {
                skipped(s)
            }
        })
        .collect();
    Ok(CalibrationOutcome {
        key: request.key,
        tag: request.tag,
        submitted: request.submitted,
        action: request.action.clone(),
        stages,
        publish_rows: Some(rows),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Months;
    use geode_core::calibration::{BUILTIN_CALIBRATION, CALIBRATION_DOC, from_doc};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::query::QueryKey;

    const UNDERLYING: &str = "SPX";
    const SPOT: f64 = 5000.0;
    const QUOTE_TIME: &str = "2026-11-06T14:00:00Z";
    const EXPIRY_MONTHS: [u32; 14] = [1, 2, 3, 6, 9, 12, 18, 24, 30, 36, 42, 48, 54, 60];
    const TRUE_REPO: f64 = 0.005;
    const DIVIDEND: f64 = 15.0;

    fn pipeline() -> Pipeline {
        let doc = merge_docs(
            CALIBRATION_DOC,
            &[LayerDoc::builtin(CALIBRATION_DOC, BUILTIN_CALIBRATION).unwrap()],
        );
        let (pipelines, _) = from_doc(&doc);
        assert_eq!(pipelines.pipelines.len(), 1);
        pipelines.pipelines.into_iter().next().unwrap()
    }

    fn quote_day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 6).unwrap()
    }

    fn months_later(months: u32) -> NaiveDate {
        quote_day().checked_add_months(Months::new(months)).unwrap()
    }

    fn funding(t: f64) -> f64 {
        0.04 + 0.002 * t / 5.0
    }

    /// Quarterly dividends for five years, from one month out.
    fn dividend_dates() -> Vec<NaiveDate> {
        (0..20).map(|q| months_later(1 + 3 * q)).collect()
    }

    fn dividends() -> DocumentRows {
        let dates = dividend_dates();
        let n = dates.len();
        DocumentRows {
            key: vec![UNDERLYING.to_string()],
            attributes: vec![],
            axes: vec![(
                "dividend_id".into(),
                Column::Utf8((0..n).map(|i| format!("d{i}")).collect()),
            )],
            values: vec![
                ("ex_date".into(), Column::Date(dates.clone())),
                ("announced_date".into(), Column::Date(dates.clone())),
                ("pay_date".into(), Column::Date(dates)),
                ("amount".into(), Column::F64(vec![DIVIDEND; n])),
                ("status".into(), Column::Utf8(vec!["confirmed".into(); n])),
            ],
        }
    }

    fn pv_dividends(t: f64) -> f64 {
        dividend_dates()
            .iter()
            .filter(|d| year_fraction(quote_day(), **d) <= t)
            .count() as f64
            * DIVIDEND
    }

    /// One expiry's chain: 20 strikes from 76% to 123.5% of spot (16 inside
    /// the builtin 0.8–1.2 window) and a forward consistent with the
    /// fixture's funding curve, dividends and `TRUE_REPO`, scaled by
    /// `forward_scale`.
    fn chain_doc(months: u32, forward_scale: f64) -> DocumentRows {
        let expiry = months_later(months);
        let t = year_fraction(quote_day(), expiry);
        let forward =
            (SPOT - pv_dividends(t)) * ((funding(t) - TRUE_REPO) * t).exp() * forward_scale;
        let strikes: Vec<f64> = (0..20).map(|i| SPOT * (0.76 + 0.025 * i as f64)).collect();
        let width = 10.0 + 4.0 * t.sqrt();
        let mut bid = Vec::new();
        let mut ask = Vec::new();
        let mut vols = Vec::new();
        for k in &strikes {
            let m = k / SPOT;
            let mid = SPOT * 0.05 * t.sqrt() * (-6.0 * (m - 1.0).abs()).exp();
            bid.push((mid - width / 2.0).max(0.0));
            ask.push(mid + width / 2.0);
            vols.push(0.2 + 0.1 * (1.0 - m));
        }
        DocumentRows {
            key: vec![
                UNDERLYING.to_string(),
                expiry.format("%Y-%m-%d").to_string(),
            ],
            attributes: vec![
                ("forward".into(), Value::F64(forward)),
                ("spot_ref".into(), Value::F64(SPOT)),
                ("quote_time".into(), Value::Utf8(QUOTE_TIME.into())),
            ],
            axes: vec![("strike".into(), Column::F64(strikes))],
            values: vec![
                ("bid_vol".into(), Column::F64(vols.clone())),
                ("ask_vol".into(), Column::F64(vols.clone())),
                ("mid_vol".into(), Column::F64(vols)),
                ("bid".into(), Column::F64(bid)),
                ("ask".into(), Column::F64(ask)),
            ],
        }
    }

    fn chain(forward_scale: f64) -> Vec<Arc<DocumentRows>> {
        EXPIRY_MONTHS
            .iter()
            .map(|m| Arc::new(chain_doc(*m, forward_scale)))
            .collect()
    }

    fn rates() -> DocumentRows {
        let tenors = vec![0.25, 0.5, 1.0, 2.0, 3.0, 5.0, 7.0, 10.0];
        let values = tenors.iter().map(|t| funding(*t)).collect();
        DocumentRows {
            key: vec![UNDERLYING.to_string(), "funding".to_string()],
            attributes: vec![("published_time".into(), Value::Utf8(QUOTE_TIME.into()))],
            axes: vec![("tenor".into(), Column::F64(tenors))],
            values: vec![("value".into(), Column::F64(values))],
        }
    }

    fn inputs(chain: Vec<Arc<DocumentRows>>) -> Vec<StageInput> {
        vec![
            StageInput {
                stage: "chain".into(),
                rows: chain,
            },
            StageInput {
                stage: "dividends".into(),
                rows: vec![Arc::new(dividends())],
            },
            StageInput {
                stage: "rates".into(),
                rows: vec![Arc::new(rates())],
            },
        ]
    }

    fn request(action: RunAction, chain: Vec<Arc<DocumentRows>>) -> CalibrationRequest {
        CalibrationRequest {
            key: QueryKey(7),
            tag: 1,
            submitted: Instant::now(),
            pipeline: pipeline(),
            underlying: UNDERLYING.to_string(),
            inputs: inputs(chain),
            action,
        }
    }

    fn run(engine: &DemoForwardCalibrator, action: RunAction) -> CalibrationOutcome {
        engine.run(&request(action, chain(1.0))).unwrap()
    }

    fn full() -> RunAction {
        RunAction::Run { from: None }
    }

    fn stage<'a>(outcome: &'a CalibrationOutcome, name: &str) -> &'a StageResult {
        outcome
            .stages
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no stage {name}"))
    }

    fn fit<'a>(outcome: &'a CalibrationOutcome, name: &str) -> &'a Fit {
        match &stage(outcome, name).payload {
            StagePayload::Fit(f) => f,
            other => panic!("{name} payload is {other:?}"),
        }
    }

    fn points<'a>(outcome: &'a CalibrationOutcome, name: &str) -> &'a TermPoints {
        match &stage(outcome, name).payload {
            StagePayload::TermPoints(p) => p,
            other => panic!("{name} payload is {other:?}"),
        }
    }

    /// The fit's curve read at `t`.
    fn fit_at(fit: &Fit, t: f64) -> f64 {
        kalman::interpolate(&fit.tenors, &fit.values, t)
    }

    /// Everything an outcome says except the clock: name, status, summary
    /// and payload per stage, and the publish rows without `published_time`.
    fn comparable(outcome: &CalibrationOutcome) -> String {
        let mut text = String::new();
        for s in &outcome.stages {
            text.push_str(&format!(
                "{} {:?} {} {:?}\n",
                s.name, s.status, s.summary, s.payload
            ));
        }
        if let Some(rows) = &outcome.publish_rows {
            for r in rows {
                text.push_str(&format!("{:?} {:?} {:?}\n", r.key, r.axes, r.values));
            }
        }
        text
    }

    const ALL: [&str; 12] = [
        "chain",
        "dividends",
        "rates",
        "parity",
        "implied_rates",
        "df_monotone",
        "repo_fit",
        "box_fit",
        "repo_curve",
        "box_curve",
        "publish",
        "",
    ];

    fn assert_statuses(outcome: &CalibrationOutcome, ok: &[&str], skipped: &[&str]) {
        for name in ok {
            assert_eq!(stage(outcome, name).status, StageStatus::Ok, "{name}");
        }
        for name in skipped {
            let s = stage(outcome, name);
            assert_eq!(s.status, StageStatus::Skipped, "{name}");
            assert!(
                matches!(s.payload, StagePayload::None),
                "{name} has a payload"
            );
            assert!(s.ran_at.is_none(), "{name} has a ran_at");
        }
    }

    #[test]
    fn a_full_run_reports_every_stage_ok_with_fits_and_rasters() {
        let engine = DemoForwardCalibrator::new();
        assert_eq!(engine.name(), DEMO_CALIBRATOR);
        let outcome = run(&engine, full());
        assert_eq!(outcome.stages.len(), 11);
        assert_statuses(&outcome, &ALL[..11], &[]);
        assert!(outcome.publish_rows.is_none());
        for s in &outcome.stages {
            assert!(s.ran_at.is_some(), "{} ran", s.name);
        }
        assert_eq!(stage(&outcome, "chain").summary, "14 documents");
        let parity = points(&outcome, "parity");
        assert_eq!(parity.tenors.len(), 14);
        assert!(parity.tenors.windows(2).all(|w| w[0] < w[1]));
        let names: Vec<&str> = parity.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["forward", "df"]);
        assert_eq!(stage(&outcome, "parity").summary, "14 of 14 expiries");
        let rates = points(&outcome, "implied_rates");
        let names: Vec<&str> = rates.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["repo", "box"]);
        for s in &rates.series {
            for i in 0..14 {
                assert!(s.lo[i] < s.mid[i] && s.mid[i] < s.hi[i], "{} {i}", s.name);
                assert!(
                    s.mid[i] > 0.0 && s.mid[i] < 0.05,
                    "{} at {:.2}y is {:.4}, outside 0–5%",
                    s.name,
                    rates.tenors[i],
                    s.mid[i]
                );
            }
        }
        let repo = &rates.series[0];
        let bx = &rates.series[1];
        assert!(repo.mid.iter().zip(&bx.mid).all(|(r, b)| r != b));
        // The fixture's forwards embed a 0.5% repo; the parity residual
        // moves each implied point a little off it.
        for (t, r) in rates.tenors.iter().zip(&repo.mid) {
            assert!((r - TRUE_REPO).abs() < 0.01, "repo at {t:.2}y is {r:.4}");
        }
        let repo_fit = fit(&outcome, "repo_fit");
        assert_eq!(repo_fit.series, "repo");
        let knots: Vec<&str> = repo_fit.params.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(knots, ["0.25", "0.50", "1.00", "2.00", "3.00", "5.00"]);
        assert_eq!(repo_fit.tenors.len(), 60);
        assert!((repo_fit.tenors[0] - 1.0 / 12.0).abs() < 1e-12);
        assert!((repo_fit.tenors[59] - 5.0).abs() < 1e-12);
        assert!(repo_fit.stats.rmse < 0.01);
        assert!(fit(&outcome, "box_fit").series == "box");
        for name in ["repo_curve", "box_curve"] {
            match &stage(&outcome, name).payload {
                StagePayload::Raster(r) => {
                    assert_eq!(r.tenors.len(), 60);
                    assert_eq!(r.values.len(), 60);
                    assert!(r.values.iter().all(|v| v.is_finite()));
                }
                other => panic!("{name} payload is {other:?}"),
            }
        }
        match &stage(&outcome, "df_monotone").payload {
            StagePayload::Check(c) => assert!(c.violations.is_empty(), "{c:?}"),
            other => panic!("check payload is {other:?}"),
        }
        assert!(matches!(
            stage(&outcome, "publish").payload,
            StagePayload::PublishReady
        ));
        assert_eq!(stage(&outcome, "publish").summary, "2 series → sophis");
    }

    #[test]
    fn an_expiry_with_too_few_strikes_is_skipped_by_name_and_the_stage_stays_ok() {
        let engine = DemoForwardCalibrator::new();
        let mut docs = chain(1.0);
        // The last expiry keeps four strikes inside the window; the rest
        // sit beyond it.
        let last = Arc::make_mut(docs.last_mut().unwrap());
        let strikes: Vec<f64> = (0..20)
            .map(|i| {
                if i < 4 {
                    SPOT * (0.9 + 0.05 * i as f64)
                } else {
                    SPOT * (1.3 + 0.05 * i as f64)
                }
            })
            .collect();
        last.axes = vec![("strike".into(), Column::F64(strikes))];
        let expiry = last.key[1].clone();
        let outcome = engine.run(&request(full(), docs)).unwrap();
        let parity = stage(&outcome, "parity");
        assert_eq!(parity.status, StageStatus::Ok);
        assert_eq!(
            parity.summary,
            format!("13 of 14 expiries · {expiry} skipped: 4 strikes")
        );
        assert_eq!(points(&outcome, "parity").tenors.len(), 13);
        assert_eq!(stage(&outcome, "publish").status, StageStatus::Ok);

        // Every expiry too thin: the stage fails and names them.
        let engine = DemoForwardCalibrator::new();
        let docs: Vec<Arc<DocumentRows>> = chain(1.0)
            .into_iter()
            .map(|d| {
                let mut d = (*d).clone();
                d.axes = vec![(
                    "strike".into(),
                    Column::F64((0..20).map(|i| SPOT * (1.3 + 0.05 * i as f64)).collect()),
                )];
                Arc::new(d)
            })
            .collect();
        let outcome = engine.run(&request(full(), docs)).unwrap();
        match &stage(&outcome, "parity").status {
            StageStatus::Failed(reason) => {
                assert!(
                    reason.starts_with("no expiry has 6 strikes inside 0.80–1.20"),
                    "{reason}"
                );
                assert!(reason.contains("skipped: 0 strikes"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn run_from_a_stage_skips_everything_not_downstream_of_it() {
        let engine = DemoForwardCalibrator::new();
        run(&engine, full());
        let outcome = run(
            &engine,
            RunAction::Run {
                from: Some("implied_rates".into()),
            },
        );
        assert_statuses(
            &outcome,
            &[
                "implied_rates",
                "repo_fit",
                "box_fit",
                "repo_curve",
                "box_curve",
                "publish",
            ],
            &["chain", "dividends", "rates", "parity", "df_monotone"],
        );
        assert!(matches!(
            stage(&outcome, "implied_rates").payload,
            StagePayload::TermPoints(_)
        ));

        // From a middle stage before anything ran: the upstream payload is
        // missing, so the stage fails by name and its consumers are skipped.
        let fresh = DemoForwardCalibrator::new();
        let outcome = run(
            &fresh,
            RunAction::Run {
                from: Some("repo_fit".into()),
            },
        );
        assert_eq!(
            stage(&outcome, "repo_fit").status,
            StageStatus::Failed("input implied_rates has not run".into())
        );
        assert_statuses(
            &outcome,
            &[],
            &[
                "chain",
                "parity",
                "implied_rates",
                "box_fit",
                "repo_curve",
                "publish",
            ],
        );
        assert!(matches!(
            fresh.run(&request(
                RunAction::Run {
                    from: Some("nowhere".into())
                },
                chain(1.0)
            )),
            Err(CalibrationError(e)) if e == "no stage nowhere in implied_forwards"
        ));
    }

    #[test]
    fn a_second_run_moves_the_filter_less_than_the_points_moved() {
        let engine = DemoForwardCalibrator::new();
        let first = run(&engine, full());
        // Forwards half a percent higher: every implied repo falls.
        let second = engine.run(&request(full(), chain(1.005))).unwrap();
        let p1 = points(&first, "implied_rates");
        let p2 = points(&second, "implied_rates");
        let f1 = fit(&first, "repo_fit");
        let f2 = fit(&second, "repo_fit");
        let mut points_moved = 0.0;
        let mut fit_moved = 0.0;
        for (i, t) in p1.tenors.iter().enumerate() {
            let dp = p2.series[0].mid[i] - p1.series[0].mid[i];
            assert!(dp < 0.0, "repo at {t:.2}y rose");
            points_moved += dp.abs();
            fit_moved += (fit_at(f2, *t) - fit_at(f1, *t)).abs();
        }
        assert!(fit_moved > 0.0, "the filter did not move");
        assert!(
            fit_moved < points_moved,
            "fit moved {fit_moved} against points {points_moved}"
        );
        assert!(f2.stats.innovation > 0.0);
        assert_eq!(f1.stats.innovation, 0.0, "the first run has no prior");
    }

    #[test]
    fn evaluate_returns_a_fit_for_the_given_params_and_leaves_state_unchanged() {
        let engine = DemoForwardCalibrator::new();
        let control = DemoForwardCalibrator::new();
        run(&engine, full());
        run(&control, full());
        let params = vec![0.01, 0.011, 0.012, 0.013, 0.014, 0.015];
        let outcome = run(
            &engine,
            RunAction::Evaluate {
                stage: "repo_fit".into(),
                params: params.clone(),
            },
        );
        let draft = fit(&outcome, "repo_fit");
        let values: Vec<f64> = draft.params.iter().map(|(_, v)| *v).collect();
        assert_eq!(values, params);
        assert!((fit_at(draft, 1.0) - 0.012).abs() < 1e-12);
        assert!((fit_at(draft, 2.5) - 0.0135).abs() < 1e-12);
        assert!(draft.stats.rmse > 0.0, "the draft is far from the points");
        assert_statuses(
            &outcome,
            &["repo_fit"],
            &[
                "chain",
                "parity",
                "implied_rates",
                "df_monotone",
                "box_fit",
                "repo_curve",
                "publish",
            ],
        );
        // The next run answers exactly what an engine that never evaluated
        // answers.
        let after = engine.run(&request(full(), chain(1.002))).unwrap();
        let expected = control.run(&request(full(), chain(1.002))).unwrap();
        assert_eq!(comparable(&after), comparable(&expected));
        // A wrong arity or a non-fit stage is refused.
        assert!(matches!(
            engine.run(&request(
                RunAction::Evaluate {
                    stage: "repo_fit".into(),
                    params: vec![0.01]
                },
                chain(1.0)
            )),
            Err(CalibrationError(e)) if e == "repo_fit has 6 knots, got 1 params"
        ));
        assert!(matches!(
            engine.run(&request(
                RunAction::Evaluate {
                    stage: "parity".into(),
                    params: vec![]
                },
                chain(1.0)
            )),
            Err(CalibrationError(e)) if e == "parity is a parity_fit stage, not kalman_fit"
        ));
    }

    #[test]
    fn seed_replaces_the_filter_mean_and_reruns_its_consumers() {
        let seeded = DemoForwardCalibrator::new();
        let plain = DemoForwardCalibrator::new();
        run(&seeded, full());
        run(&plain, full());
        let seed = vec![0.10; 6];
        let outcome = run(
            &seeded,
            RunAction::Seed {
                stage: "repo_fit".into(),
                params: seed.clone(),
            },
        );
        assert_statuses(
            &outcome,
            &["repo_fit", "repo_curve", "publish"],
            &[
                "chain",
                "dividends",
                "rates",
                "parity",
                "implied_rates",
                "df_monotone",
                "box_fit",
                "box_curve",
            ],
        );
        let unseeded = run(&plain, full());
        let after_seed = fit(&outcome, "repo_fit");
        let after_plain = fit(&unseeded, "repo_fit");
        for (i, (name, v)) in after_seed.params.iter().enumerate() {
            let plain_v = after_plain.params[i].1;
            assert_ne!(*v, plain_v, "knot {name}");
            assert!(
                (v - seed[i]).abs() < (plain_v - seed[i]).abs(),
                "knot {name}: seeded run {v} is no closer to the seed than {plain_v}"
            );
        }
        // The raster downstream carries the seeded fit.
        match &stage(&outcome, "repo_curve").payload {
            StagePayload::Raster(r) => assert_eq!(r.values, after_seed.values),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn publish_from_draft_returns_rate_curve_rows_for_each_series() {
        let engine = DemoForwardCalibrator::new();
        let ran = run(&engine, full());
        let params = vec![0.02, 0.021, 0.022, 0.023, 0.024, 0.025];
        let outcome = run(
            &engine,
            RunAction::Publish {
                from: PublishSource::Draft {
                    stage: "repo_fit".into(),
                    params: params.clone(),
                },
            },
        );
        let rows = outcome.publish_rows.as_ref().expect("publish rows");
        assert_eq!(rows.len(), 2);
        let repo = &rows[0];
        assert_eq!(repo.key, vec!["SPX".to_string(), "repo".to_string()]);
        assert_eq!(rows[1].key, vec!["SPX".to_string(), "box".to_string()]);
        for row in rows {
            assert_eq!(row.axes.len(), 1);
            assert_eq!(row.axes[0].0, "tenor");
            assert_eq!(row.values.len(), 1);
            assert_eq!(row.values[0].0, "value");
            assert_eq!(row.axes[0].1.len(), 60);
            assert_eq!(row.values[0].1.len(), 60);
            match &row.attributes[..] {
                [(name, Value::Utf8(time))] => {
                    assert_eq!(name, "published_time");
                    assert!(DateTime::parse_from_rfc3339(time).is_ok(), "{time}");
                }
                other => panic!("{other:?}"),
            }
        }
        let Column::F64(values) = &repo.values[0].1 else {
            panic!()
        };
        assert!((values[11] - 0.022).abs() < 1e-12, "the draft's 1y knot");
        assert!((values[59] - 0.025).abs() < 1e-12, "the draft's 5y knot");
        let Column::F64(bx) = &rows[1].values[0].1 else {
            panic!()
        };
        assert_eq!(
            *bx,
            fit(&ran, "box_fit").values,
            "box comes from the filter"
        );
        assert_statuses(
            &outcome,
            &["repo_curve", "box_curve", "publish"],
            &[
                "chain",
                "parity",
                "implied_rates",
                "df_monotone",
                "repo_fit",
                "box_fit",
            ],
        );
        // From the filter: both series as they stand.
        let filter = run(
            &engine,
            RunAction::Publish {
                from: PublishSource::Filter,
            },
        );
        let rows = filter.publish_rows.unwrap();
        let Column::F64(values) = &rows[0].values[0].1 else {
            panic!()
        };
        assert_eq!(*values, fit(&ran, "repo_fit").values);
    }

    #[test]
    fn publish_is_refused_while_the_publish_stage_is_held() {
        let engine = DemoForwardCalibrator::new();
        let publish = || {
            engine.run(&request(
                RunAction::Publish {
                    from: PublishSource::Filter,
                },
                chain(1.0),
            ))
        };
        assert_eq!(
            publish().unwrap_err(),
            CalibrationError("no run of implied_forwards has happened for SPX".into())
        );
        for _ in 0..7 {
            run(&engine, full());
        }
        let err = publish().unwrap_err();
        assert_eq!(
            err,
            CalibrationError("publish is held: df_monotone: 1 violation".into())
        );
        run(&engine, full()); // run 8, still held
        assert!(publish().is_err());
        run(&engine, full()); // run 9 clears it
        assert!(publish().is_ok());
    }

    #[test]
    fn the_check_holds_publish_on_runs_seven_and_eight_of_fourteen() {
        let engine = DemoForwardCalibrator::new();
        for run_index in 1..=16u32 {
            let outcome = run(&engine, full());
            let check = stage(&outcome, "df_monotone");
            let publish = stage(&outcome, "publish");
            let tenor = points(&outcome, "parity").tenors[1];
            if run_index == 7 || run_index == 8 {
                assert_eq!(
                    publish.status,
                    StageStatus::Held("df_monotone: 1 violation".into()),
                    "run {run_index}"
                );
                assert!(matches!(publish.payload, StagePayload::None));
                assert_eq!(
                    check.summary,
                    format!(
                        "1 violation · df rises at {tenor:.2}y (forced on run {run_index} of 14)"
                    )
                );
                assert_eq!(check.status, StageStatus::Ok, "the check itself ran");
            } else {
                assert_eq!(publish.status, StageStatus::Ok, "run {run_index}");
                assert_eq!(check.summary, "passed · 14 tenors", "run {run_index}");
            }
        }
    }

    #[test]
    fn a_failed_stage_skips_its_consumers() {
        let engine = DemoForwardCalibrator::new();
        // No chain documents at all: parity fails, everything downstream
        // is skipped, the other sources still ran.
        let outcome = engine.run(&request(full(), Vec::new())).unwrap();
        assert_eq!(
            stage(&outcome, "parity").status,
            StageStatus::Failed("chain has no documents".into())
        );
        assert_statuses(
            &outcome,
            &["chain", "dividends", "rates"],
            &[
                "implied_rates",
                "df_monotone",
                "repo_fit",
                "box_fit",
                "repo_curve",
                "box_curve",
                "publish",
            ],
        );
        assert_eq!(stage(&outcome, "chain").summary, "0 documents");
    }

    #[test]
    fn a_wrong_payload_at_a_port_fails_the_stage_naming_the_kinds() {
        // A pipeline `from_doc` would refuse: the check reads a fit.
        let mut pipeline = pipeline();
        let check = pipeline
            .stages
            .iter_mut()
            .find(|s| s.name == "df_monotone")
            .unwrap();
        check.inputs = vec!["repo_fit".into()];
        let engine = DemoForwardCalibrator::new();
        let mut req = request(full(), chain(1.0));
        req.pipeline = pipeline;
        let outcome = engine.run(&req).unwrap();
        assert_eq!(
            stage(&outcome, "df_monotone").status,
            StageStatus::Failed("input repo_fit is a fit, not term points".into())
        );
        assert_eq!(stage(&outcome, "publish").status, StageStatus::Skipped);
    }

    #[test]
    fn the_engine_is_deterministic() {
        let script = |engine: &DemoForwardCalibrator| -> Vec<String> {
            let mut out = vec![
                comparable(&run(engine, full())),
                comparable(&engine.run(&request(full(), chain(1.003))).unwrap()),
            ];
            out.push(comparable(&run(
                engine,
                RunAction::Evaluate {
                    stage: "box_fit".into(),
                    params: vec![0.02; 6],
                },
            )));
            out.push(comparable(&run(
                engine,
                RunAction::Seed {
                    stage: "repo_fit".into(),
                    params: vec![0.01; 6],
                },
            )));
            out.push(comparable(&run(engine, full())));
            out.push(comparable(&run(
                engine,
                RunAction::Publish {
                    from: PublishSource::Filter,
                },
            )));
            out
        };
        let a = script(&DemoForwardCalibrator::new());
        let b = script(&DemoForwardCalibrator::new());
        assert_eq!(a, b);
        // And the sequence is not degenerate: runs differ from one another.
        assert_ne!(a[0], a[1]);
        assert_ne!(a[1], a[4]);
    }
}
