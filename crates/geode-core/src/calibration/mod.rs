//! Calibration pipelines from `calibration.toml`, pure.
//!
//! A pipeline is a DAG of named stages. Stages are top-level objects of the
//! `stages` table, each naming its `pipeline`; the `inputs` list is the only
//! edge representation and may name only stages of the same pipeline. Stage
//! kinds are a closed set with typed ports, so a document is checked for
//! port arity and payload type before anything runs. `doc::from_doc` reads
//! and validates the merged document; `doc::check_against` adds the checks
//! that need the schema and the egress targets.

pub mod doc;

pub use crate::config::CALIBRATION_DOC;
pub use doc::{check_against, from_doc};

/// The builtin document: the one `implied_forwards` pipeline.
pub const BUILTIN_CALIBRATION: &str = include_str!("builtin.toml");

pub type StageName = String;

/// When a pipeline runs: on every input change, or only when asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    OnInput,
    Manual,
}

/// What flows along an edge. A port accepts exactly one payload type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadType {
    Chain,
    Schedule,
    Curve,
    TermPoints,
    Fit,
    Raster,
    Check,
    PublishReady,
}

/// The `measurement_noise` of a Kalman fit: the quoted spread of each point,
/// or one positive number for every point.
#[derive(Debug, Clone, PartialEq)]
pub enum Noise {
    Spread,
    Fixed(f64),
}

/// A stage's kind and its kind-specific fields. The set is closed: a
/// document naming any other kind is refused.
#[derive(Debug, Clone, PartialEq)]
pub enum StageKind {
    Source {
        dataset: String,
        series: Option<String>,
    },
    ParityFit {
        min_strikes: usize,
        strike_window: (f64, f64),
    },
    ImpliedRates,
    /// `check` is `"monotone_df"` only.
    Check {
        check: String,
    },
    KalmanFit {
        series: String,
        knots: Vec<f64>,
        process_noise: f64,
        measurement_noise: Noise,
    },
    /// `grid` is `"monthly_to_5y"` only.
    Rasterize {
        grid: String,
    },
    Publish {
        document: String,
        target: String,
    },
}

/// The input ports of a kind: a fixed list matched to `inputs` by position,
/// or a variadic set taking one or more `required` payloads plus any number
/// of `gates`.
#[derive(Debug, Clone, PartialEq)]
pub enum Ports {
    Fixed(Vec<(&'static str, PayloadType)>),
    Variadic {
        required: PayloadType,
        gates: Option<PayloadType>,
    },
}

impl StageKind {
    /// The payload the stage emits. A source emits what its dataset holds:
    /// `option_chain` a `Chain`, `dividend_schedule` a `Schedule`, and any
    /// other document a `Curve`.
    pub fn output(&self) -> PayloadType {
        match self {
            StageKind::Source { dataset, .. } => match dataset.as_str() {
                "option_chain" => PayloadType::Chain,
                "dividend_schedule" => PayloadType::Schedule,
                _ => PayloadType::Curve,
            },
            StageKind::ParityFit { .. } | StageKind::ImpliedRates => PayloadType::TermPoints,
            StageKind::Check { .. } => PayloadType::Check,
            StageKind::KalmanFit { .. } => PayloadType::Fit,
            StageKind::Rasterize { .. } => PayloadType::Raster,
            StageKind::Publish { .. } => PayloadType::PublishReady,
        }
    }

    pub fn ports(&self) -> Ports {
        match self {
            StageKind::Source { .. } => Ports::Fixed(Vec::new()),
            StageKind::ParityFit { .. } => Ports::Fixed(vec![("chain", PayloadType::Chain)]),
            StageKind::ImpliedRates => Ports::Fixed(vec![
                ("parity", PayloadType::TermPoints),
                ("dividends", PayloadType::Schedule),
                ("rates", PayloadType::Curve),
            ]),
            StageKind::Check { .. } => Ports::Fixed(vec![("points", PayloadType::TermPoints)]),
            StageKind::KalmanFit { .. } => Ports::Fixed(vec![("points", PayloadType::TermPoints)]),
            StageKind::Rasterize { .. } => Ports::Fixed(vec![("fit", PayloadType::Fit)]),
            StageKind::Publish { .. } => Ports::Variadic {
                required: PayloadType::Raster,
                gates: Some(PayloadType::Check),
            },
        }
    }

    /// The `kind` word the document spells.
    pub fn kind_name(&self) -> &'static str {
        match self {
            StageKind::Source { .. } => "source",
            StageKind::ParityFit { .. } => "parity_fit",
            StageKind::ImpliedRates => "implied_rates",
            StageKind::Check { .. } => "check",
            StageKind::KalmanFit { .. } => "kalman_fit",
            StageKind::Rasterize { .. } => "rasterize",
            StageKind::Publish { .. } => "publish",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stage {
    pub name: StageName,
    pub kind: StageKind,
    /// The stages this one reads, in port order. Every name is a stage of
    /// the same pipeline once `from_doc` has accepted the pipeline.
    pub inputs: Vec<StageName>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pipeline {
    pub name: String,
    /// Defaults to the name.
    pub title: String,
    /// Defaults to `OnInput`.
    pub run: RunMode,
    /// Document order.
    pub stages: Vec<Stage>,
}

impl Pipeline {
    pub fn stage(&self, name: &str) -> Option<&Stage> {
        self.stages.iter().find(|s| s.name == name)
    }

    /// The stages whose `inputs` name `name`, in document order.
    pub fn consumers(&self, name: &str) -> Vec<&Stage> {
        self.stages
            .iter()
            .filter(|s| s.inputs.iter().any(|i| i == name))
            .collect()
    }

    /// Every stage in an order each stage follows all of its inputs: first
    /// every stage with none (the sources), then every stage whose inputs
    /// are all placed, and so on, each round in document order. Inputs that
    /// name no stage of this pipeline are ignored, and a stage a cycle keeps
    /// from ever being placed is appended in document order, so the
    /// answer always holds every stage even for a pipeline `from_doc` would
    /// have refused.
    pub fn topological(&self) -> Vec<&Stage> {
        let mut placed = vec![false; self.stages.len()];
        let mut order: Vec<&Stage> = Vec::with_capacity(self.stages.len());
        let index = |name: &str| self.stages.iter().position(|s| s.name == name);
        loop {
            let ready: Vec<usize> = (0..self.stages.len())
                .filter(|&i| !placed[i])
                .filter(|&i| {
                    self.stages[i]
                        .inputs
                        .iter()
                        .all(|input| index(input).is_none_or(|j| placed[j]))
                })
                .collect();
            if ready.is_empty() {
                break;
            }
            for i in ready {
                placed[i] = true;
                order.push(&self.stages[i]);
            }
        }
        order.extend(
            self.stages
                .iter()
                .enumerate()
                .filter(|(i, _)| !placed[*i])
                .map(|(_, s)| s),
        );
        order
    }
}

/// The pipelines a document declares, in document order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pipelines {
    pub pipelines: Vec<Pipeline>,
}
