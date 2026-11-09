//! Shared vocabulary for running a calibration pipeline: the request the
//! data tier carries, the per-stage outcome a tile paints, and the
//! `Calibrator` trait an engine implements. Implementations live in
//! `geode-pricing` (the simulator) or a vendor crate; the shell routes
//! `CalibrationOutcome` without depending on either, which is why the trait
//! lives here, as `vol::VolModel` does.
//!
//! The app does no calibration arithmetic: every point, parameter and
//! rasterized value a tile paints came out of one of these results.

use super::{Pipeline, StageName};
use crate::document::DocumentRows;
use crate::query::QueryKey;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The name of the single synthetic stage in an outcome the engine never
/// produced: a request the queue refused, one for an engine the binary
/// lacks, one the engine answered with an error, or one whose engine call
/// panicked. Such an outcome has this one stage, `Failed(reason)` with no
/// `ran_at`, so a tile recognises it by name and shows a notice rather
/// than a stage card. No pipeline stage may use the name; `from_doc`
/// refuses one.
pub const REQUEST_STAGE: &str = "request";

/// An engine that runs one pipeline over its source documents. `run` is
/// calculation and never a module's to do; it holds any filter state the
/// engine keeps between runs.
pub trait Calibrator: Send + Sync {
    fn name(&self) -> &str;
    fn run(&self, request: &CalibrationRequest) -> Result<CalibrationOutcome, CalibrationError>;
}

/// One run, addressed by request key and submission tag exactly as a
/// `VolSliceParams` is. It carries the pipeline itself, so the engine
/// needs no configuration of its own.
#[derive(Debug, Clone)]
pub struct CalibrationRequest {
    pub key: QueryKey,
    pub tag: u64,
    pub submitted: Instant,
    pub pipeline: Pipeline,
    /// The underlying every source document belongs to.
    pub underlying: String,
    /// The source documents, one entry per source stage.
    pub inputs: Vec<StageInput>,
    pub action: RunAction,
}

/// The documents a source stage read. A chain has one per expiry, a
/// curve one per series; a schedule has one.
#[derive(Debug, Clone)]
pub struct StageInput {
    pub stage: StageName,
    pub rows: Vec<Arc<DocumentRows>>,
}

/// What the run does to the pipeline's filter state.
#[derive(Debug, Clone, PartialEq)]
pub enum RunAction {
    /// Run every stage, or every stage from `from` downstream.
    Run { from: Option<StageName> },
    /// Compute `stage` at `params` without touching the filter state.
    Evaluate { stage: StageName, params: Vec<f64> },
    /// Replace the filter state of `stage` with `params`, then run
    /// downstream.
    Seed { stage: StageName, params: Vec<f64> },
    /// Produce the publish rows from `from`.
    Publish { from: PublishSource },
}

/// Where a publish takes its values: the filter state as it stands, or a
/// draft of one stage at given parameters.
#[derive(Debug, Clone, PartialEq)]
pub enum PublishSource {
    Filter,
    Draft { stage: StageName, params: Vec<f64> },
}

/// The queue a request waits in. `Background` runs follow input changes
/// and may be replaced by a newer one; `User` actions answer a keypress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lane {
    Background,
    User,
}

impl RunAction {
    /// `Run` is the background lane; every other action is the user's.
    pub fn lane(&self) -> Lane {
        match self {
            RunAction::Run { .. } => Lane::Background,
            RunAction::Evaluate { .. } | RunAction::Seed { .. } | RunAction::Publish { .. } => {
                Lane::User
            }
        }
    }

    /// Whether a newer request for the same key may supersede this one
    /// before it runs. `Run` and `Evaluate` are idempotent reads of the
    /// state; `Seed` and `Publish` change or export it and must each run.
    pub fn replaceable(&self) -> bool {
        matches!(self, RunAction::Run { .. } | RunAction::Evaluate { .. })
    }
}

/// The answer to one request. `stages` holds one entry per stage the
/// action touched and no other: `Run { from: None }` reports every stage
/// of the pipeline; `Run { from: Some(s) }` and `Seed { stage: s, .. }`
/// report `s` and its transitive consumers; `Evaluate` reports the
/// evaluated stage alone; `Publish` reports the rasterize stages it read
/// and the publish stage. A stage absent from the list was not touched and
/// its last card stands, so a tile painting each delivered stage never
/// blanks a card the action did not reach.
#[derive(Debug, Clone)]
pub struct CalibrationOutcome {
    pub key: QueryKey,
    pub tag: u64,
    pub submitted: Instant,
    pub action: RunAction,
    pub stages: Vec<StageResult>,
    /// One document per rasterized series, for `RunAction::Publish` only.
    pub publish_rows: Option<Vec<DocumentRows>>,
}

/// One stage's result: what the tile's card shows.
#[derive(Debug, Clone)]
pub struct StageResult {
    pub name: StageName,
    pub status: StageStatus,
    /// The wall-clock UTC time the stage finished; `None` when it did not
    /// run.
    pub ran_at: Option<DateTime<Utc>>,
    pub elapsed: Duration,
    /// The card's third line, engine-worded.
    pub summary: String,
    pub payload: StagePayload,
}

/// How a stage ended. `Held` is a check that refused to pass its gate.
/// `Skipped` is a stage the action selected but did not run because a
/// stage upstream of it in the same action `Failed` (or was itself
/// skipped for that reason); a stage the action did not select is absent
/// from [`CalibrationOutcome::stages`], never `Skipped`. `Stale` is a
/// stage the run did not reach.
#[derive(Debug, Clone, PartialEq)]
pub enum StageStatus {
    Ok,
    Failed(String),
    Held(String),
    Skipped,
    Stale,
}

/// What a stage emitted, matched to its kind's output payload type.
#[derive(Debug, Clone)]
pub enum StagePayload {
    None,
    TermPoints(TermPoints),
    Fit(Fit),
    Raster(Raster),
    Check(CheckResult),
    PublishReady,
}

/// Points on the tenor axis, one series each with a mid and a lo..hi bar.
#[derive(Debug, Clone, Default)]
pub struct TermPoints {
    pub tenors: Vec<f64>,
    pub series: Vec<Series>,
}

/// One series of term points, parallel to `TermPoints::tenors`.
#[derive(Debug, Clone)]
pub struct Series {
    pub name: String,
    pub mid: Vec<f64>,
    pub lo: Vec<f64>,
    pub hi: Vec<f64>,
}

/// A fitted curve: its named parameters, the curve sampled at `tenors`,
/// and how well it fits the points.
#[derive(Debug, Clone)]
pub struct Fit {
    pub series: String,
    /// Parameter name and value; the knot tenors name a knots basis.
    pub params: Vec<(String, f64)>,
    pub tenors: Vec<f64>,
    pub values: Vec<f64>,
    pub stats: FitStats,
}

/// Fit quality against the input points, plus the last innovation norm.
#[derive(Debug, Clone, Default)]
pub struct FitStats {
    pub rmse: f64,
    pub max_residual: f64,
    pub max_residual_tenor: f64,
    pub innovation: f64,
}

/// A curve sampled onto the publish grid.
#[derive(Debug, Clone)]
pub struct Raster {
    pub series: String,
    pub tenors: Vec<f64>,
    pub values: Vec<f64>,
}

/// A check's findings; empty when it passed.
#[derive(Debug, Clone)]
pub struct CheckResult {
    pub violations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationError(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_is_the_background_lane_and_every_other_action_the_user_lane() {
        assert_eq!(RunAction::Run { from: None }.lane(), Lane::Background);
        assert_eq!(
            RunAction::Run {
                from: Some("parity".into())
            }
            .lane(),
            Lane::Background
        );
        assert_eq!(
            RunAction::Evaluate {
                stage: "fit".into(),
                params: vec![0.1]
            }
            .lane(),
            Lane::User
        );
        assert_eq!(
            RunAction::Seed {
                stage: "fit".into(),
                params: vec![0.1]
            }
            .lane(),
            Lane::User
        );
        assert_eq!(
            RunAction::Publish {
                from: PublishSource::Filter
            }
            .lane(),
            Lane::User
        );
        assert_eq!(
            RunAction::Publish {
                from: PublishSource::Draft {
                    stage: "fit".into(),
                    params: vec![0.1]
                }
            }
            .lane(),
            Lane::User
        );
    }

    #[test]
    fn only_run_and_evaluate_are_replaceable() {
        assert!(RunAction::Run { from: None }.replaceable());
        assert!(
            RunAction::Evaluate {
                stage: "fit".into(),
                params: vec![]
            }
            .replaceable()
        );
        assert!(
            !RunAction::Seed {
                stage: "fit".into(),
                params: vec![]
            }
            .replaceable()
        );
        assert!(
            !RunAction::Publish {
                from: PublishSource::Filter
            }
            .replaceable()
        );
        assert!(
            !RunAction::Publish {
                from: PublishSource::Draft {
                    stage: "fit".into(),
                    params: vec![]
                }
            }
            .replaceable()
        );
    }
}
