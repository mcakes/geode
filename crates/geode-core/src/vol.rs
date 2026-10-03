//! Shared vocabulary for evaluating a vol surface document into slices:
//! requests, results, the batch the data tier carries, and the `VolModel`
//! trait an evaluator implements. Implementations live in `geode-pricing`
//! (the demo stand-in) or a vendor crate; the shell routes `VolSliceOutcome`
//! without depending on either, which is why the trait lives here, as
//! `pricing::Pricer` does.
//!
//! The app does no vol arithmetic: every coordinate value, vol and density
//! a tile paints came out of one of these results.

use crate::document::DocumentRows;
use crate::query::QueryKey;
use chrono::NaiveDate;
use std::sync::Arc;
use std::time::Instant;

/// The x coordinate a slice is expressed in. Every value is computed by
/// the model (strike over forward is not the module's to divide).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Coordinate {
    Strike,
    #[default]
    Moneyness,
    LogMoneyness,
    /// Black call delta of the point at its own vol.
    Delta,
}

impl Coordinate {
    pub const ALL: [Coordinate; 4] = [
        Coordinate::Strike,
        Coordinate::Moneyness,
        Coordinate::LogMoneyness,
        Coordinate::Delta,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Coordinate::Strike => "strike",
            Coordinate::Moneyness => "moneyness",
            Coordinate::LogMoneyness => "log-moneyness",
            Coordinate::Delta => "delta",
        }
    }

    /// The `:x <name>` spelling; `log` is accepted for log-moneyness.
    pub fn parse(text: &str) -> Option<Coordinate> {
        match text.trim().to_ascii_lowercase().as_str() {
            "strike" => Some(Coordinate::Strike),
            "moneyness" => Some(Coordinate::Moneyness),
            "log-moneyness" | "log" => Some(Coordinate::LogMoneyness),
            "delta" => Some(Coordinate::Delta),
            _ => None,
        }
    }
}

/// Where a slice is evaluated. `Dense` is `n` strictly ascending strikes
/// from end to end of the union of the document's own strike range for
/// that expiry and `cover`, the model choosing the spacing (the demo model
/// packs them toward the forward so a short-dated density stays smooth); `At` is absolute strikes, in any order, echoed
/// back in the same order. `Job(j)` is the strikes the batch's earlier
/// `Slice` job `j` evaluated at: the vol worker resolves it to `At` before
/// the model sees the request, so two curves can be compared at equal
/// strikes in one batch. Density over an `At` grid is meaningful only for
/// ascending strikes.
#[derive(Debug, Clone, PartialEq)]
pub enum Grid {
    /// `cover` is an absolute strike span `(lo, hi)` the grid must reach,
    /// such as a listed chain's lowest and highest strike, so a curve is
    /// drawn at least as wide as the quotes beside it. It only widens: a
    /// cover inside the document's own range leaves the grid as it was.
    /// Naming strikes is not evaluating them; how a model reads a strike
    /// past its own range is the model's to say. A cover that is not an
    /// ascending pair of positive, finite strikes fails the job.
    Dense {
        n: usize,
        cover: Option<(f64, f64)>,
    },
    At(Vec<f64>),
    Job(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SliceRequest {
    pub expiry: NaiveDate,
    pub coordinate: Coordinate,
    pub grid: Grid,
    pub density: bool,
}

/// One evaluated point: the strike it was evaluated at, that strike in
/// the requested coordinate, and the vol.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlicePoint {
    pub strike: f64,
    pub x: f64,
    pub vol: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SliceResult {
    pub expiry: NaiveDate,
    pub forward: f64,
    /// In the grid's order: ascending strike for `Dense`, as given for `At`.
    pub points: Vec<SlicePoint>,
    /// `(x, pdf)` at the grid's interior points, when asked for. The pdf
    /// is per unit of `x`, the requested coordinate, so its area over any
    /// coordinate is about one; a point where `x` does not move between
    /// its neighbours (delta saturating at 0 or 1) is `NaN`.
    pub density: Option<Vec<(f64, f64)>>,
}

/// Place points that already have vols (a chain) in a coordinate, using
/// the chain's own vols and forward. `as_of` is the quote date the time
/// to expiry is measured from.
#[derive(Debug, Clone, PartialEq)]
pub struct MapRequest {
    pub expiry: NaiveDate,
    pub as_of: NaiveDate,
    pub forward: f64,
    pub coordinate: Coordinate,
    pub strikes: Vec<f64>,
    pub vols: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolError(pub String);

/// An evaluator of one document kind. `slice` evaluates a document at
/// any expiry inside its term range and `coordinates` places given
/// points; both are calculation and neither is a module's to do.
pub trait VolModel: Send + Sync {
    fn name(&self) -> &str;
    /// The document kind this model reads, e.g. `cvi_params`: what a
    /// viewer must fetch to feed it. Informational; a wrong document
    /// fails in `slice` with the missing column named.
    fn kind(&self) -> &str;
    fn slice(&self, doc: &DocumentRows, req: &SliceRequest) -> Result<SliceResult, VolError>;
    fn coordinates(&self, req: &MapRequest) -> Result<Vec<f64>, VolError>;
}

/// One job of a batch. `Slice` names its document by index into
/// [`VolSliceParams::documents`].
#[derive(Debug, Clone, PartialEq)]
pub enum VolJob {
    Slice {
        document: usize,
        request: SliceRequest,
    },
    Map(MapRequest),
}

/// A batch, addressed by request key and submission tag exactly as a
/// `PriceParams` is. One batch carries every job a repaint needs.
#[derive(Debug, Clone)]
pub struct VolSliceParams {
    pub key: QueryKey,
    pub tag: u64,
    pub submitted: Instant,
    pub documents: Vec<Arc<DocumentRows>>,
    pub jobs: Vec<VolJob>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VolResult {
    Slice(SliceResult),
    Map(Vec<f64>),
}

/// The answer, one entry per job in request order. A cancelled batch
/// carries the jobs finished before the cancel landed.
#[derive(Debug)]
pub struct VolSliceOutcome {
    pub key: QueryKey,
    pub tag: u64,
    pub submitted: Instant,
    pub results: Vec<Result<VolResult, String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_round_trip_through_their_names() {
        for c in Coordinate::ALL {
            assert_eq!(Coordinate::parse(c.name()), Some(c));
        }
        assert_eq!(Coordinate::parse("delta"), Some(Coordinate::Delta));
        assert_eq!(Coordinate::parse("log"), Some(Coordinate::LogMoneyness));
        assert_eq!(Coordinate::parse("nope"), None);
        assert_eq!(Coordinate::default(), Coordinate::Moneyness);
    }
}
