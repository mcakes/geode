//! The demo vol model. Deterministic, smooth and NOT a model: it exists
//! so a vol slice has a curve that moves when a CVI cell is edited, and
//! it stands in for the desk's evaluator behind the same `VolModel`
//! trait.
//!
//! How it reads `cvi_params`: a `node` is a percent moneyness offset
//! (`k = node/100`, strike `F(1+k)`); a term's knot vol is
//! `atm + skew·k + param/100`; the smile is the natural cubic spline
//! through a term's knots; between terms total variance is linear in
//! time at equal `k` and the forward log-linear. Outside the term range
//! the nearest term's smile holds flat in vol at equal `k` (total
//! variance grows with time and never turns down), and the forward keeps
//! the nearest pair's log-linear carry: before the first term that pair
//! is the document's `spot_ref` at time zero and the first term, past
//! the last it is the last two terms, and with no pair the end term's
//! forward holds. Past a term's node ladder in
//! strike the smile continues the spline's end slope, and a vol never
//! goes below `VOL_FLOOR`. A dense grid spans the ladder, widened to a
//! request's cover, its points packed toward the forward.

use crate::spline::Spline;
use chrono::NaiveDate;
use geode_core::document::{Column, DocumentRows, Value};
use geode_core::vol::{
    Coordinate, Grid, MapRequest, SlicePoint, SliceRequest, SliceResult, VolError, VolModel,
};

/// The name `[vol] model` selects, and its default.
pub const DEMO_VOL_MODEL: &str = "demo";
/// The document kind this model reads.
pub const KIND: &str = "cvi_params";
/// No vol below this, whatever the extrapolated smile says.
pub const VOL_FLOOR: f64 = 0.01;
/// A dense grid's sinh scale is at least the span over this: the most it
/// packs points toward the forward is about this many times its wings'
/// spacing (`cosh(asinh(DENSE_SPREAD/2))`).
const DENSE_SPREAD: f64 = 50.0;
/// Time to expiry never below half a day, so a term on the anchor
/// evaluates and still sits before a term the day after it.
const MIN_DAYS: f64 = 0.5;

#[derive(Debug, Clone, Copy, Default)]
pub struct DemoVolModel;

struct Term {
    date: NaiveDate,
    t: f64,
    forward: f64,
    smile: Spline,
    k_min: f64,
    k_max: f64,
}

struct Surface {
    /// Sorted by date.
    terms: Vec<Term>,
    anchor: NaiveDate,
    /// The `spot_ref` attribute when it is a positive number: the forward
    /// at time zero, which the carry before the first term runs from.
    spot: Option<f64>,
}

/// What a slice at one expiry evaluates through: a forward and a vol
/// function of moneyness offset `k`.
struct Curve {
    t: f64,
    forward: f64,
    k_min: f64,
    k_max: f64,
    vol: Box<dyn Fn(f64) -> f64>,
}

fn f64_column<'a>(
    cols: &'a [(String, Column)],
    name: &str,
    what: &str,
) -> Result<&'a [f64], VolError> {
    match cols.iter().find(|(n, _)| n == name) {
        Some((_, Column::F64(v))) => Ok(v),
        Some(_) => Err(VolError(format!(
            "{KIND} document {what} {name} is not numeric"
        ))),
        None => Err(VolError(format!("{KIND} document lacks the {what} {name}"))),
    }
}

/// Finite and above zero; `!(x > 0.0)` would also be right but trips
/// clippy's partial-order lint.
fn positive(x: f64) -> bool {
    x.is_finite() && x > 0.0
}

/// Days over 365, with every date on or before the anchor at
/// `MIN_DAYS`: order-preserving from the anchor onwards (0 days is half
/// a day, 1 day is one day), while terms before the anchor collapse and
/// are refused by `Surface::read` as coincident.
fn year_fraction(anchor: NaiveDate, date: NaiveDate) -> f64 {
    let days = (date - anchor).num_days();
    let days = if days <= 0 { MIN_DAYS } else { days as f64 };
    days / 365.0
}

impl Surface {
    fn read(doc: &DocumentRows) -> Result<Surface, VolError> {
        let anchor = match doc.attributes.iter().find(|(n, _)| n == "anchor_date") {
            Some((_, Value::Date(d))) => *d,
            Some(_) => {
                return Err(VolError(format!(
                    "{KIND} document attribute anchor_date is not a date"
                )));
            }
            None => {
                return Err(VolError(format!(
                    "{KIND} document lacks the attribute anchor_date"
                )));
            }
        };
        let terms = match doc.axes.iter().find(|(n, _)| n == "term") {
            Some((_, Column::Date(v))) => v,
            Some(_) => return Err(VolError(format!("{KIND} document axis term is not a date"))),
            None => return Err(VolError(format!("{KIND} document lacks the axis term"))),
        };
        let nodes = f64_column(&doc.axes, "node", "axis")?;
        let params = f64_column(&doc.values, "param", "column")?;
        let forwards = f64_column(&doc.values, "forward", "column")?;
        let atms = f64_column(&doc.values, "atm", "column")?;
        let skews = f64_column(&doc.values, "skew", "column")?;
        let n = terms.len();
        if [
            nodes.len(),
            params.len(),
            forwards.len(),
            atms.len(),
            skews.len(),
        ]
        .iter()
        .any(|l| *l != n)
        {
            return Err(VolError(format!(
                "{KIND} document columns differ in length"
            )));
        }
        if n == 0 {
            return Err(VolError(format!("{KIND} document has no terms")));
        }
        // Group rows by term in date order; rows within a term keep their
        // order, then knots are sorted by k for the spline.
        let mut dates: Vec<NaiveDate> = terms.to_vec();
        dates.sort();
        dates.dedup();
        let mut out = Vec::with_capacity(dates.len());
        for date in dates {
            let rows: Vec<usize> = (0..n).filter(|i| terms[*i] == date).collect();
            // Strike F(1+k) is non-positive at k <= -1.
            if rows.iter().any(|&i| nodes[i] <= -100.0) {
                return Err(VolError(format!(
                    "{KIND} document term {date} has a node at or below -100%"
                )));
            }
            let first = rows[0];
            let (forward, atm, skew) = (forwards[first], atms[first], skews[first]);
            let mut knots: Vec<(f64, f64)> = rows
                .iter()
                .map(|&i| {
                    let k = nodes[i] / 100.0;
                    (k, atm + skew * k + params[i] / 100.0)
                })
                .collect();
            knots.sort_by(|a, b| a.0.total_cmp(&b.0));
            knots.dedup_by(|a, b| a.0 == b.0);
            if knots.len() < 2 {
                return Err(VolError(format!(
                    "{KIND} document term {date} has fewer than two nodes"
                )));
            }
            if !positive(forward) {
                return Err(VolError(format!(
                    "{KIND} document term {date} has a non-positive forward"
                )));
            }
            let (ks, vs): (Vec<f64>, Vec<f64>) = knots.iter().copied().unzip();
            let smile = Spline::natural(&ks, &vs).ok_or_else(|| {
                VolError(format!("{KIND} document term {date} has no usable nodes"))
            })?;
            out.push(Term {
                date,
                t: year_fraction(anchor, date),
                forward,
                smile,
                k_min: ks[0],
                k_max: ks[ks.len() - 1],
            });
        }
        // Distinct dates can floor to the same year fraction (two terms
        // before the anchor); interpolating between them would divide by
        // zero, so refuse the document up front.
        if let Some(pair) = out.windows(2).find(|w| w[1].t <= w[0].t) {
            return Err(VolError(format!(
                "{KIND} document terms {} and {} are not distinct in time",
                pair[0].date, pair[1].date
            )));
        }
        let spot = match doc.attributes.iter().find(|(n, _)| n == "spot_ref") {
            Some((_, Value::F64(v))) if positive(*v) => Some(*v),
            _ => None,
        };
        Ok(Surface {
            terms: out,
            anchor,
            spot,
        })
    }

    fn curve_at(self, expiry: NaiveDate) -> Result<Curve, VolError> {
        let first = self.terms[0].date;
        let last = self.terms[self.terms.len() - 1].date;
        if expiry < first || expiry > last {
            return Ok(self.extrapolated(expiry));
        }
        let mut terms = self.terms;
        if let Some(i) = terms.iter().position(|t| t.date == expiry) {
            let term = terms.swap_remove(i);
            let smile = term.smile;
            return Ok(Curve {
                t: term.t,
                forward: term.forward,
                k_min: term.k_min,
                k_max: term.k_max,
                vol: Box::new(move |k| smile.eval(k).max(VOL_FLOOR)),
            });
        }
        let hi = terms
            .iter()
            .position(|t| t.date > expiry)
            .expect("inside the range");
        let b = terms.swap_remove(hi);
        let a = terms.swap_remove(hi - 1);
        let t = a.t
            + (b.t - a.t) * ((expiry - a.date).num_days() as f64)
                / ((b.date - a.date).num_days() as f64);
        let w = (t - a.t) / (b.t - a.t);
        let forward = (a.forward.ln() + (b.forward.ln() - a.forward.ln()) * w).exp();
        let (ta, tb) = (a.t, b.t);
        let (sa, sb) = (a.smile, b.smile);
        Ok(Curve {
            t,
            forward,
            k_min: a.k_min.min(b.k_min),
            k_max: a.k_max.max(b.k_max),
            vol: Box::new(move |k| {
                let va = sa.eval(k).max(VOL_FLOOR);
                let vb = sb.eval(k).max(VOL_FLOOR);
                let wa = va * va * ta;
                let wb = vb * vb * tb;
                ((wa + (wb - wa) * w) / t).sqrt().max(VOL_FLOOR)
            }),
        })
    }
}

impl Surface {
    /// An expiry before the first term or after the last: the end term's
    /// smile flat in vol at equal `k` over its own ladder, and the forward
    /// carried log-linearly in time from the nearest pair (module doc).
    fn extrapolated(mut self, expiry: NaiveDate) -> Curve {
        let t = year_fraction(self.anchor, expiry);
        let before = expiry < self.terms[0].date;
        // The pair the carry runs through, as (t, forward) at each end.
        let pair = if before {
            let first = &self.terms[0];
            self.spot.map(|s| ((0.0, s), (first.t, first.forward)))
        } else {
            let n = self.terms.len();
            (n >= 2).then(|| {
                let (a, b) = (&self.terms[n - 2], &self.terms[n - 1]);
                ((a.t, a.forward), (b.t, b.forward))
            })
        };
        let end = if before {
            self.terms.swap_remove(0)
        } else {
            self.terms.pop().expect("a surface has terms")
        };
        let forward = match pair {
            Some(((ta, fa), (tb, fb))) => {
                let carry = (fb.ln() - fa.ln()) / (tb - ta);
                (end.forward.ln() + carry * (t - end.t)).exp()
            }
            None => end.forward,
        };
        let smile = end.smile;
        Curve {
            t,
            forward,
            k_min: end.k_min,
            k_max: end.k_max,
            vol: Box::new(move |k| smile.eval(k).max(VOL_FLOOR)),
        }
    }
}

impl Curve {
    /// The moneyness offsets a dense grid spans: the node ladder (for an
    /// expiry between terms, the union of the bracketing terms' ladders),
    /// widened to reach `cover`'s strikes. Past the ladder the smile is the
    /// spline's linear continuation of its end slope, floored at
    /// `VOL_FLOOR`, so a steep wing can meet the floor and kink there.
    fn span(&self, cover: Option<(f64, f64)>) -> Result<(f64, f64), VolError> {
        let Some((lo, hi)) = cover else {
            return Ok((self.k_min, self.k_max));
        };
        if !(positive(lo) && positive(hi) && lo <= hi) {
            return Err(VolError(format!(
                "cover {lo}..{hi} is not an ascending pair of positive strikes"
            )));
        }
        Ok((
            self.k_min.min(lo / self.forward - 1.0),
            self.k_max.max(hi / self.forward - 1.0),
        ))
    }

    /// `n >= 2` moneyness offsets from `k_min` to `k_max`, both exact,
    /// strictly ascending, packed toward the forward: `k = c·sinh(u)` with
    /// `u` even between `asinh(k_min/c)` and `asinh(k_max/c)`. The scale
    /// `c` is the at-the-money σ√t, where the density has its width, so a
    /// short-dated density keeps its points per σ√t·F however wide the
    /// cover; it is floored at `1/DENSE_SPREAD` of the span, so a 0DTE
    /// expiry over a wide chain still reaches its wings, and a long-dated
    /// one (σ√t a large part of the span) is nearly even.
    fn dense_offsets(&self, k_min: f64, k_max: f64, n: usize) -> Vec<f64> {
        let c = ((self.vol)(0.0) * self.t.sqrt()).max((k_max - k_min) / DENSE_SPREAD);
        let (u0, u1) = ((k_min / c).asinh(), (k_max / c).asinh());
        let last = n - 1;
        (0..n)
            .map(|i| match i {
                0 => k_min,
                i if i == last => k_max,
                i => c * (u0 + (u1 - u0) * i as f64 / last as f64).sinh(),
            })
            .collect()
    }

    fn strikes(&self, grid: &Grid) -> Result<Vec<f64>, VolError> {
        Ok(match grid {
            Grid::Dense { n, cover } => {
                let (k_min, k_max) = self.span(*cover)?;
                match *n {
                    0 => Vec::new(),
                    1 => vec![self.forward * (1.0 + k_min)],
                    n => self
                        .dense_offsets(k_min, k_max, n)
                        .into_iter()
                        .map(|k| self.forward * (1.0 + k))
                        .collect(),
                }
            }
            Grid::At(strikes) => {
                if let Some(bad) = strikes.iter().find(|k| !positive(**k)) {
                    return Err(VolError(format!("strike {bad} is not positive")));
                }
                strikes.clone()
            }
            Grid::Job(_) => {
                return Err(VolError(
                    "a job grid is resolved by the vol worker, not the model".into(),
                ));
            }
        })
    }

    fn vol_at_strike(&self, strike: f64) -> f64 {
        (self.vol)(strike / self.forward - 1.0)
    }
}

/// The coordinate value of a strike with a given vol; shared by `slice`
/// and `coordinates` so a chain point and a curve point at the same
/// strike are placed by the same arithmetic.
fn coordinate_of(coordinate: Coordinate, forward: f64, strike: f64, vol: f64, t: f64) -> f64 {
    match coordinate {
        Coordinate::Strike => strike,
        Coordinate::Moneyness => strike / forward,
        Coordinate::LogMoneyness => (strike / forward).ln(),
        Coordinate::Delta => crate::black::call_delta(forward, strike, vol, t),
    }
}

impl VolModel for DemoVolModel {
    fn name(&self) -> &str {
        DEMO_VOL_MODEL
    }

    fn kind(&self) -> &str {
        KIND
    }

    fn slice(&self, doc: &DocumentRows, req: &SliceRequest) -> Result<SliceResult, VolError> {
        let curve = Surface::read(doc)?.curve_at(req.expiry)?;
        let strikes = curve.strikes(&req.grid)?;
        // The second difference below assumes a strike step that is
        // positive between neighbours; a dense grid always ascends.
        if req.density && strikes.windows(2).any(|w| w[1] <= w[0]) {
            return Err(VolError(
                "density needs strictly ascending strikes".to_string(),
            ));
        }
        let points: Vec<SlicePoint> = strikes
            .iter()
            .map(|&strike| {
                let vol = curve.vol_at_strike(strike);
                SlicePoint {
                    strike,
                    x: coordinate_of(req.coordinate, curve.forward, strike, vol, curve.t),
                    vol,
                }
            })
            .collect();
        let density = req
            .density
            .then(|| density(&points, curve.forward, curve.t));
        Ok(SliceResult {
            expiry: req.expiry,
            forward: curve.forward,
            points,
            density,
        })
    }

    fn coordinates(&self, req: &MapRequest) -> Result<Vec<f64>, VolError> {
        if req.strikes.len() != req.vols.len() {
            return Err(VolError(format!(
                "map has {} strikes and {} vols",
                req.strikes.len(),
                req.vols.len()
            )));
        }
        if !positive(req.forward) {
            return Err(VolError(format!("forward {} is not positive", req.forward)));
        }
        if let Some(bad) = req.strikes.iter().find(|k| !positive(**k)) {
            return Err(VolError(format!("strike {bad} is not positive")));
        }
        let t = year_fraction(req.as_of, req.expiry);
        Ok(req
            .strikes
            .iter()
            .zip(&req.vols)
            .map(|(&strike, &vol)| {
                coordinate_of(req.coordinate, req.forward, strike, vol.max(VOL_FLOOR), t)
            })
            .collect())
    }
}

/// Breeden–Litzenberger on the slice's own points: the second
/// difference of the undiscounted call price in strike, at each interior
/// point, on a grid that need not be uniform. Nothing is clamped: a
/// negative lobe is a butterfly violation the trader wants to see.
fn density(points: &[SlicePoint], forward: f64, t: f64) -> Vec<(f64, f64)> {
    if points.len() < 3 {
        return Vec::new();
    }
    let prices: Vec<f64> = points
        .iter()
        .map(|p| crate::black::call_price(forward, p.strike, p.vol, t))
        .collect();
    (1..points.len() - 1)
        .map(|i| {
            let h0 = points[i].strike - points[i - 1].strike;
            let h1 = points[i + 1].strike - points[i].strike;
            let pdf = 2.0 * ((prices[i + 1] - prices[i]) / h1 - (prices[i] - prices[i - 1]) / h0)
                / (h0 + h1);
            // Per unit x: the pdf in strike times |dK/dx|, by the central
            // difference over the same neighbours. Exact for strike and
            // moneyness (linear in K); a flat stretch of x (delta at 0 or
            // 1) has no finite Jacobian and reads NaN, a gap, rather than
            // a spike.
            let jacobian = ((points[i + 1].strike - points[i - 1].strike)
                / (points[i + 1].x - points[i - 1].x))
                .abs();
            let pdf = if jacobian.is_finite() {
                pdf * jacobian
            } else {
                f64::NAN
            };
            (points[i].x, pdf)
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::NaiveDate;
    use geode_core::document::{Column, Value};

    pub(crate) fn date(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    pub(crate) const NODES: [f64; 6] = [-20.0, -10.0, -5.0, 0.0, 2.5, 5.0];

    /// A CVI document: `terms` are `(date, forward, atm, skew)`, params
    /// come from `param(node, term_index)`. Rows are term-major, the
    /// shape the parser produces.
    pub(crate) fn cvi_doc(
        anchor: &str,
        terms: &[(&str, f64, f64, f64)],
        param: impl Fn(f64, usize) -> f64,
    ) -> DocumentRows {
        let mut term_col = Vec::new();
        let mut node_col = Vec::new();
        let mut params = Vec::new();
        let mut fwd = Vec::new();
        let mut atm = Vec::new();
        let mut skew = Vec::new();
        for (ti, (d, f, a, s)) in terms.iter().enumerate() {
            for n in NODES {
                term_col.push(date(d));
                node_col.push(n);
                params.push(param(n, ti));
                fwd.push(*f);
                atm.push(*a);
                skew.push(*s);
            }
        }
        DocumentRows {
            key: vec!["SPX.Z".into()],
            attributes: vec![
                ("anchor_date".into(), Value::Date(date(anchor))),
                ("spot_ref".into(), Value::F64(100.0)),
            ],
            axes: vec![
                ("term".into(), Column::Date(term_col)),
                ("node".into(), Column::F64(node_col)),
            ],
            values: vec![
                ("param".into(), Column::F64(params)),
                ("forward".into(), Column::F64(fwd)),
                ("atm".into(), Column::F64(atm)),
                ("skew".into(), Column::F64(skew)),
            ],
        }
    }

    pub(crate) fn flat(atm: f64) -> DocumentRows {
        cvi_doc(
            "2026-09-01",
            &[
                ("2026-10-16", 100.0, atm, 0.0),
                ("2027-04-16", 100.0, atm, 0.0),
            ],
            |_, _| 0.0,
        )
    }

    fn at(expiry: &str, strikes: &[f64]) -> SliceRequest {
        SliceRequest {
            expiry: date(expiry),
            coordinate: Coordinate::Strike,
            grid: Grid::At(strikes.to_vec()),
            density: false,
        }
    }

    #[test]
    fn at_a_term_the_slice_reproduces_the_knot_vols() {
        let doc = cvi_doc(
            "2026-09-01",
            &[
                ("2026-10-16", 100.0, 0.20, -0.8),
                ("2026-12-18", 101.0, 0.22, -0.6),
            ],
            |n, ti| 0.5 * n.abs() + ti as f64,
        );
        let strikes: Vec<f64> = NODES.iter().map(|n| 101.0 * (1.0 + n / 100.0)).collect();
        let r = DemoVolModel
            .slice(&doc, &at("2026-12-18", &strikes))
            .unwrap();
        assert_eq!(r.expiry, date("2026-12-18"));
        assert!((r.forward - 101.0).abs() < 1e-12);
        for (p, n) in r.points.iter().zip(NODES) {
            let k = n / 100.0;
            let expected = 0.22 + -0.6 * k + (0.5 * n.abs() + 1.0) / 100.0;
            assert!(
                (p.vol - expected).abs() < 1e-9,
                "node {n}: {} vs {expected}",
                p.vol
            );
            assert!(
                (p.x - p.strike).abs() < 1e-12,
                "strike coordinate echoes the strike"
            );
        }
    }

    #[test]
    fn between_terms_total_variance_is_linear_in_time() {
        // Two flat terms at 0.2 and 0.4; the midpoint in time is not the
        // midpoint in vol but the root of the mean variance-time.
        let doc = cvi_doc(
            "2026-09-01",
            &[
                ("2026-10-01", 100.0, 0.2, 0.0),
                ("2026-12-01", 100.0, 0.4, 0.0),
            ],
            |_, _| 0.0,
        );
        let r = DemoVolModel
            .slice(&doc, &at("2026-11-01", &[100.0]))
            .unwrap();
        let ta: f64 = 30.0 / 365.0;
        let tb = 91.0 / 365.0;
        let t = 61.0 / 365.0;
        let w = 0.04 * ta + (0.16 * tb - 0.04 * ta) * (t - ta) / (tb - ta);
        let expected = (w / t).sqrt();
        assert!(
            (r.points[0].vol - expected).abs() < 1e-9,
            "{} vs {expected}",
            r.points[0].vol
        );
        // Same-vol terms interpolate to that vol exactly.
        let same = flat(0.25);
        let r = DemoVolModel
            .slice(&same, &at("2026-12-25", &[80.0, 100.0, 120.0]))
            .unwrap();
        assert!(
            r.points.iter().all(|p| (p.vol - 0.25).abs() < 1e-9),
            "{:?}",
            r.points
        );
    }

    #[test]
    fn the_forward_is_log_linear_between_terms() {
        let doc = cvi_doc(
            "2026-09-01",
            &[
                ("2026-10-01", 100.0, 0.2, 0.0),
                ("2026-12-01", 400.0, 0.2, 0.0),
            ],
            |_, _| 0.0,
        );
        let r = DemoVolModel
            .slice(&doc, &at("2026-11-01", &[100.0]))
            .unwrap();
        let ta = 30.0 / 365.0;
        let tb = 91.0 / 365.0;
        let t = 61.0 / 365.0;
        let expected =
            (100.0f64.ln() + (400.0f64.ln() - 100.0f64.ln()) * (t - ta) / (tb - ta)).exp();
        assert!(
            (r.forward - expected).abs() < 1e-6,
            "{} vs {expected}",
            r.forward
        );
    }

    #[test]
    fn outside_the_terms_the_end_smile_holds_flat_in_vol_at_equal_k() {
        // Terms 2026-10-01 and 2026-12-01 with different smiles: before the
        // first, the first's vols at equal k; past the last, the last's.
        let doc = cvi_doc(
            "2026-09-01",
            &[
                ("2026-10-01", 100.0, 0.20, -0.8),
                ("2026-12-01", 104.0, 0.30, -0.4),
            ],
            |n, ti| 0.3 * n.abs() + ti as f64,
        );
        for (expiry, term) in [("2026-09-10", "2026-10-01"), ("2027-06-01", "2026-12-01")] {
            let ks = [-0.2, -0.05, 0.0, 0.05];
            let out = DemoVolModel.slice(&doc, &at(expiry, &[1.0])).unwrap();
            let end = DemoVolModel.slice(&doc, &at(term, &[1.0])).unwrap();
            let strikes = |f: f64| ks.iter().map(|k| f * (1.0 + k)).collect::<Vec<_>>();
            let out = DemoVolModel
                .slice(&doc, &at(expiry, &strikes(out.forward)))
                .unwrap();
            let end = DemoVolModel
                .slice(&doc, &at(term, &strikes(end.forward)))
                .unwrap();
            for (o, e) in out.points.iter().zip(&end.points) {
                assert!((o.vol - e.vol).abs() < 1e-9, "{expiry}: {o:?} vs {e:?}");
            }
        }
    }

    #[test]
    fn outside_the_terms_the_forward_keeps_the_nearest_carry() {
        let doc = cvi_doc(
            "2026-09-01",
            &[
                ("2026-10-01", 102.0, 0.2, 0.0),
                ("2026-12-01", 110.0, 0.2, 0.0),
            ],
            |_, _| 0.0,
        );
        let t = |d: &str| (date(d) - date("2026-09-01")).num_days() as f64 / 365.0;
        let fwd = |e: &str| DemoVolModel.slice(&doc, &at(e, &[100.0])).unwrap().forward;
        // Past the last term: the last two terms' carry continues.
        let carry = (110.0f64 / 102.0).ln() / (t("2026-12-01") - t("2026-10-01"));
        let expected = 110.0 * (carry * (t("2027-03-01") - t("2026-12-01"))).exp();
        assert!((fwd("2027-03-01") - expected).abs() < 1e-9);
        // Before the first: from spot_ref (100) at the anchor to the first.
        let carry = (102.0f64 / 100.0).ln() / t("2026-10-01");
        let expected = 100.0 * (carry * t("2026-09-15")).exp();
        assert!((fwd("2026-09-15") - expected).abs() < 1e-9);
        // With no spot_ref the first term's forward holds before it.
        let mut bare = doc.clone();
        bare.attributes.retain(|(n, _)| n != "spot_ref");
        let r = DemoVolModel
            .slice(&bare, &at("2026-09-15", &[100.0]))
            .unwrap();
        assert_eq!(r.forward, 102.0);
    }

    #[test]
    fn a_one_term_document_holds_its_forward_past_the_term() {
        let doc = cvi_doc("2026-09-01", &[("2026-10-01", 102.0, 0.2, 0.0)], |_, _| 0.0);
        let r = DemoVolModel
            .slice(&doc, &at("2027-01-15", &[100.0]))
            .unwrap();
        assert_eq!(r.forward, 102.0);
        assert!((r.points[0].vol - 0.2).abs() < 1e-9);
    }

    #[test]
    fn an_atm_edit_lifts_every_point_by_the_same_amount() {
        let a = cvi_doc(
            "2026-09-01",
            &[
                ("2026-10-16", 100.0, 0.20, -0.8),
                ("2027-01-15", 100.0, 0.21, -0.7),
            ],
            |n, _| 0.3 * n,
        );
        let mut b = a.clone();
        let Column::F64(atm) = &mut b.values[2].1 else {
            panic!()
        };
        for v in atm.iter_mut() {
            *v += 0.05;
        }
        let req = SliceRequest {
            expiry: date("2026-11-20"),
            coordinate: Coordinate::Strike,
            grid: Grid::Dense { n: 50, cover: None },
            density: false,
        };
        // The atm vol sets a dense grid's spacing, so b is read at a's
        // strikes.
        let ra = DemoVolModel.slice(&a, &req).unwrap();
        let strikes: Vec<f64> = ra.points.iter().map(|p| p.strike).collect();
        let rb = DemoVolModel.slice(&b, &at("2026-11-20", &strikes)).unwrap();
        assert_eq!(ra.points.len(), 50);
        // Variance interpolation makes the lift approximate between terms;
        // it is exact at a term, and monotone everywhere.
        for (pa, pb) in ra.points.iter().zip(&rb.points) {
            assert!((pa.strike - pb.strike).abs() < 1e-9);
            assert!(
                pb.vol > pa.vol + 0.04 && pb.vol < pa.vol + 0.06,
                "{} -> {}",
                pa.vol,
                pb.vol
            );
        }
        let at_term = SliceRequest {
            expiry: date("2026-10-16"),
            ..req
        };
        let ra = DemoVolModel.slice(&a, &at_term).unwrap();
        let strikes: Vec<f64> = ra.points.iter().map(|p| p.strike).collect();
        let rb = DemoVolModel.slice(&b, &at("2026-10-16", &strikes)).unwrap();
        for (pa, pb) in ra.points.iter().zip(&rb.points) {
            assert!((pb.vol - pa.vol - 0.05).abs() < 1e-9);
        }
    }

    #[test]
    fn a_dense_grid_spans_the_node_ladder_ascending() {
        let doc = flat(0.2);
        let r = DemoVolModel
            .slice(
                &doc,
                &SliceRequest {
                    expiry: date("2026-10-16"),
                    coordinate: Coordinate::Strike,
                    grid: Grid::Dense { n: 7, cover: None },
                    density: false,
                },
            )
            .unwrap();
        let ks: Vec<f64> = r.points.iter().map(|p| p.strike).collect();
        assert!(
            (ks[0] - 80.0).abs() < 1e-9 && (ks[6] - 105.0).abs() < 1e-9,
            "{ks:?}"
        );
        assert!(ks.windows(2).all(|w| w[1] > w[0]));
    }

    fn dense(expiry: &str, n: usize, cover: Option<(f64, f64)>) -> SliceRequest {
        SliceRequest {
            expiry: date(expiry),
            coordinate: Coordinate::Strike,
            grid: Grid::Dense { n, cover },
            density: false,
        }
    }

    fn dense_strikes(doc: &DocumentRows, n: usize, cover: Option<(f64, f64)>) -> Vec<f64> {
        DemoVolModel
            .slice(doc, &dense("2026-10-16", n, cover))
            .unwrap()
            .points
            .iter()
            .map(|p| p.strike)
            .collect()
    }

    /// The ladder is 80..105 at a forward of 100: a cover past either end
    /// moves that end alone, and the grid stays strictly ascending, its
    /// finest step at the forward.
    #[test]
    fn a_dense_grid_widens_to_its_cover_on_either_side() {
        let doc = flat(0.2);
        for (cover, lo, hi) in [
            ((60.0, 130.0), 60.0, 130.0),
            ((60.0, 100.0), 60.0, 105.0),
            ((90.0, 130.0), 80.0, 130.0),
        ] {
            let ks = dense_strikes(&doc, 41, Some(cover));
            assert_eq!(ks.len(), 41);
            assert!(
                (ks[0] - lo).abs() < 1e-9 && (ks[40] - hi).abs() < 1e-9,
                "{cover:?}: {ks:?}"
            );
            assert!(ks.windows(2).all(|w| w[1] > w[0]), "{cover:?}: {ks:?}");
            let finest = ks
                .windows(2)
                .min_by(|a, b| (a[1] - a[0]).total_cmp(&(b[1] - b[0])))
                .unwrap();
            assert!(
                finest[0] <= 101.0 && finest[1] >= 99.0,
                "{cover:?}: the finest step {finest:?} is at the forward"
            );
        }
    }

    #[test]
    fn a_cover_inside_the_ladder_does_not_narrow_the_grid() {
        let doc = flat(0.2);
        let ladder = dense_strikes(&doc, 7, None);
        for cover in [(95.0, 100.0), (80.0, 105.0)] {
            let ks = dense_strikes(&doc, 7, Some(cover));
            assert!(
                ks.iter().zip(&ladder).all(|(a, b)| (a - b).abs() < 1e-9),
                "{cover:?}: {ks:?} vs {ladder:?}"
            );
        }
    }

    /// A one-week expiry over a cover of ±50% of the forward: spread
    /// evenly, 500 strikes would put some 14 across one σ√t·F (2.8% of
    /// the forward); packed toward the forward they put 25 or more, and
    /// the density there is smooth.
    #[test]
    fn a_wide_cover_keeps_the_points_per_sigma_root_t_at_the_forward() {
        let doc = cvi_doc(
            "2026-09-01",
            &[
                ("2026-09-08", 100.0, 0.2, 0.0),
                ("2026-10-16", 100.0, 0.2, 0.0),
            ],
            |_, _| 0.0,
        );
        let r = DemoVolModel
            .slice(
                &doc,
                &SliceRequest {
                    density: true,
                    ..dense("2026-09-08", 500, Some((50.0, 150.0)))
                },
            )
            .unwrap();
        let ks: Vec<f64> = r.points.iter().map(|p| p.strike).collect();
        assert_eq!((ks[0], ks[499]), (50.0, 150.0));
        assert!(ks.windows(2).all(|w| w[1] > w[0]));
        let width = 100.0 * 0.2 * (7.0f64 / 365.0).sqrt();
        let near = ks
            .iter()
            .filter(|k| (**k - 100.0).abs() <= width / 2.0)
            .count();
        assert!(near >= 25, "{near} points across one σ√t·F at the forward");
        let d = r.density.unwrap();
        let peak = d.iter().map(|(_, p)| *p).fold(f64::MIN, f64::max);
        let steepest = d
            .windows(2)
            .map(|w| (w[1].1 - w[0].1).abs())
            .fold(0.0, f64::max);
        assert!(steepest / peak < 0.025, "steepest step {}", steepest / peak);
        let mass: f64 = d
            .windows(2)
            .map(|w| 0.5 * (w[0].1 + w[1].1) * (w[1].0 - w[0].0))
            .sum();
        assert!((mass - 1.0).abs() < 0.01, "{mass}");
    }

    /// Past the ladder the smile continues its end slope: a linear smile
    /// stays linear out to the cover, floored at `VOL_FLOOR`.
    #[test]
    fn past_the_ladder_a_covered_strike_continues_the_end_slope() {
        let doc = cvi_doc(
            "2026-09-01",
            &[
                ("2026-10-16", 100.0, 0.2, -0.5),
                ("2027-01-15", 100.0, 0.2, -0.5),
            ],
            |_, _| 0.0,
        );
        let r = DemoVolModel
            .slice(&doc, &dense("2026-10-16", 9, Some((60.0, 150.0))))
            .unwrap();
        let vols: Vec<f64> = r.points.iter().map(|p| p.vol).collect();
        // k = -0.4 at the low end, k = 0.5 meets the floor at the high.
        assert!((vols[0] - 0.4).abs() < 1e-9, "{vols:?}");
        assert!((vols[8] - VOL_FLOOR).abs() < 1e-12, "{vols:?}");
        for p in &r.points {
            let k = p.strike / 100.0 - 1.0;
            let want = (0.2 - 0.5 * k).max(VOL_FLOOR);
            assert!((p.vol - want).abs() < 1e-9, "{p:?}");
        }
    }

    #[test]
    fn a_cover_that_is_not_an_ascending_pair_of_positive_strikes_is_refused() {
        let doc = flat(0.2);
        for (cover, words) in [
            ((0.0, 100.0), "cover 0..100"),
            ((-5.0, 100.0), "cover -5..100"),
            ((110.0, 90.0), "cover 110..90"),
            ((90.0, f64::INFINITY), "cover 90..inf"),
            ((f64::NAN, 100.0), "cover NaN..100"),
        ] {
            let err = DemoVolModel
                .slice(&doc, &dense("2026-10-16", 7, Some(cover)))
                .unwrap_err();
            assert_eq!(
                err.0,
                format!("{words} is not an ascending pair of positive strikes")
            );
        }
        // One strike is a span of none, and still a cover.
        assert_eq!(dense_strikes(&doc, 2, Some((100.0, 100.0))), [80.0, 105.0]);
    }

    /// A one-week expiry at about 20% vol has σ√t·F near 2.8% of the
    /// forward. Over a cover some 42% of the forward wide, a thousand
    /// strikes put about sixty points across one σ√t·F: the density is
    /// one smooth hump that integrates to one, its steepest step between
    /// neighbours a small fraction of its peak.
    #[test]
    fn a_one_week_density_over_a_wide_cover_is_smooth_and_integrates_to_one() {
        let doc = cvi_doc(
            "2026-09-01",
            &[
                ("2026-09-08", 100.0, 0.2, -0.3),
                ("2026-10-16", 100.0, 0.2, -0.3),
            ],
            |_, _| 0.0,
        );
        let r = DemoVolModel
            .slice(
                &doc,
                &SliceRequest {
                    density: true,
                    ..dense("2026-09-08", 1000, Some((70.0, 112.0)))
                },
            )
            .unwrap();
        assert!((r.points[0].strike - 70.0).abs() < 1e-9);
        assert!((r.points[999].strike - 112.0).abs() < 1e-9);
        let d = r.density.unwrap();
        let mass: f64 = d
            .windows(2)
            .map(|w| 0.5 * (w[0].1 + w[1].1) * (w[1].0 - w[0].0))
            .sum();
        assert!((mass - 1.0).abs() < 0.01, "{mass}");
        let peak = d.iter().map(|(_, p)| *p).fold(f64::MIN, f64::max);
        assert!(d.iter().all(|(_, p)| *p > -1e-9 * peak), "no negative lobe");
        // One hump: the slope changes sign once wherever the density is
        // more than a thousandth of its peak.
        let body: Vec<f64> = d
            .iter()
            .map(|(_, p)| *p)
            .filter(|p| *p > 1e-3 * peak)
            .collect();
        let flips = body
            .windows(3)
            .filter(|w| (w[1] - w[0]).signum() != (w[2] - w[1]).signum())
            .count();
        assert_eq!(flips, 1, "one peak");
        let steepest = d
            .windows(2)
            .map(|w| (w[1].1 - w[0].1).abs())
            .fold(0.0, f64::max);
        assert!(
            steepest / peak < 0.025,
            "about 25 points or more per σ√t·F: steepest step {}",
            steepest / peak
        );
    }

    #[test]
    fn a_dense_grid_of_zero_or_one_points_does_not_panic() {
        let doc = flat(0.2);
        for n in [0, 1] {
            let r = DemoVolModel
                .slice(
                    &doc,
                    &SliceRequest {
                        expiry: date("2026-10-16"),
                        coordinate: Coordinate::Moneyness,
                        grid: Grid::Dense { n, cover: None },
                        density: true,
                    },
                )
                .unwrap();
            assert_eq!(r.points.len(), n);
            assert_eq!(r.density.as_deref(), Some(&[][..]));
        }
    }

    #[test]
    fn terms_are_sorted_before_bracketing() {
        // The later term is listed first; interpolation must still bracket
        // by date, not by row order.
        let doc = cvi_doc(
            "2026-09-01",
            &[
                ("2026-12-01", 100.0, 0.4, 0.0),
                ("2026-10-01", 100.0, 0.2, 0.0),
            ],
            |_, _| 0.0,
        );
        let r = DemoVolModel
            .slice(&doc, &at("2026-10-01", &[100.0]))
            .unwrap();
        assert!((r.points[0].vol - 0.2).abs() < 1e-9);
        let r = DemoVolModel
            .slice(&doc, &at("2026-12-01", &[100.0]))
            .unwrap();
        assert!((r.points[0].vol - 0.4).abs() < 1e-9);
    }

    #[test]
    fn a_document_missing_a_column_or_the_anchor_is_refused_naming_it() {
        let mut doc = flat(0.2);
        doc.values.retain(|(n, _)| n != "skew");
        let err = DemoVolModel
            .slice(&doc, &at("2026-10-16", &[100.0]))
            .unwrap_err();
        assert_eq!(err.0, "cvi_params document lacks the column skew");
        let mut doc = flat(0.2);
        doc.attributes.clear();
        let err = DemoVolModel
            .slice(&doc, &at("2026-10-16", &[100.0]))
            .unwrap_err();
        assert_eq!(err.0, "cvi_params document lacks the attribute anchor_date");
        let empty = DocumentRows {
            key: vec!["X".into()],
            attributes: vec![("anchor_date".into(), Value::Date(date("2026-09-01")))],
            axes: vec![
                ("term".into(), Column::Date(vec![])),
                ("node".into(), Column::F64(vec![])),
            ],
            values: vec![
                ("param".into(), Column::F64(vec![])),
                ("forward".into(), Column::F64(vec![])),
                ("atm".into(), Column::F64(vec![])),
                ("skew".into(), Column::F64(vec![])),
            ],
        };
        let err = DemoVolModel
            .slice(&empty, &at("2026-10-16", &[100.0]))
            .unwrap_err();
        assert_eq!(err.0, "cvi_params document has no terms");
    }

    #[test]
    fn the_vol_floor_holds_where_the_extrapolated_smile_goes_negative() {
        // A steep skew and a far call strike drive the linear extrapolation
        // below zero; the floor keeps the point finite and positive.
        let doc = cvi_doc(
            "2026-09-01",
            &[
                ("2026-10-16", 100.0, 0.1, -3.0),
                ("2027-01-15", 100.0, 0.1, -3.0),
            ],
            |_, _| 0.0,
        );
        let r = DemoVolModel
            .slice(&doc, &at("2026-10-16", &[300.0]))
            .unwrap();
        assert!(
            (r.points[0].vol - 0.01).abs() < 1e-12,
            "{}",
            r.points[0].vol
        );
    }

    /// Total-variance interpolation of two flat terms `(days_a, vol_a)`
    /// and `(days_b, vol_b)` at `days`, the arithmetic the model states.
    fn variance_interp(days_a: f64, vol_a: f64, days_b: f64, vol_b: f64, days: f64) -> f64 {
        let (ta, tb, t) = (days_a / 365.0, days_b / 365.0, days / 365.0);
        let (wa, wb) = (vol_a * vol_a * ta, vol_b * vol_b * tb);
        ((wa + (wb - wa) * (t - ta) / (tb - ta)) / t).sqrt()
    }

    #[test]
    fn between_three_terms_the_bracketing_pair_is_chosen_by_date() {
        // Flat 0.2 / 0.4 / 0.2 at 30, 61 and 91 days. Each gap is sliced
        // at its midpoint: the first gap catches a bracket that skips the
        // middle term (its two ends agree at 0.2, so a wrong pair paints
        // exactly 0.2), the second catches a bracket pinned to the first
        // two terms (which would extrapolate past 0.4).
        let doc = cvi_doc(
            "2026-09-01",
            &[
                ("2026-10-01", 100.0, 0.2, 0.0),
                ("2026-11-01", 100.0, 0.4, 0.0),
                ("2026-12-01", 100.0, 0.2, 0.0),
            ],
            |_, _| 0.0,
        );
        let r = DemoVolModel
            .slice(&doc, &at("2026-10-16", &[100.0]))
            .unwrap();
        let expected = variance_interp(30.0, 0.2, 61.0, 0.4, 45.0);
        let skipped_middle = variance_interp(30.0, 0.2, 91.0, 0.2, 45.0);
        assert!(
            (r.points[0].vol - expected).abs() < 1e-9,
            "{} vs {expected}",
            r.points[0].vol
        );
        assert!((r.points[0].vol - skipped_middle).abs() > 1e-6);
        let r = DemoVolModel
            .slice(&doc, &at("2026-11-16", &[100.0]))
            .unwrap();
        let expected = variance_interp(61.0, 0.4, 91.0, 0.2, 76.0);
        let first_pair = variance_interp(30.0, 0.2, 61.0, 0.4, 76.0);
        assert!(
            r.points[0].vol > 0.2 && r.points[0].vol < 0.4,
            "{}",
            r.points[0].vol
        );
        assert!(
            (r.points[0].vol - expected).abs() < 1e-9,
            "{} vs {expected}",
            r.points[0].vol
        );
        assert!((r.points[0].vol - first_pair).abs() > 1e-6);
    }

    #[test]
    fn terms_that_share_a_year_fraction_are_refused() {
        // Both terms fall before the anchor and floor to half a day, so
        // interpolating between them would divide by zero; the document
        // is refused naming the pair.
        let doc = cvi_doc(
            "2026-09-01",
            &[
                ("2026-08-30", 100.0, 0.2, 0.0),
                ("2026-08-31", 100.0, 0.2, 0.0),
            ],
            |_, _| 0.0,
        );
        let err = DemoVolModel
            .slice(&doc, &at("2026-08-30", &[100.0]))
            .unwrap_err();
        assert_eq!(
            err.0,
            "cvi_params document terms 2026-08-30 and 2026-08-31 are not distinct in time"
        );
    }

    #[test]
    fn an_anchor_day_and_next_day_term_still_slice_the_far_term() {
        // Daily expiries (0DTE, 1DTE) are ordinary desk data: a term on
        // the anchor and the day after keep distinct times, so the
        // document reads and every term still slices.
        let doc = cvi_doc(
            "2026-09-01",
            &[
                ("2026-09-01", 100.0, 0.2, 0.0),
                ("2026-09-02", 100.0, 0.2, 0.0),
                ("2026-10-01", 100.0, 0.2, 0.0),
            ],
            |_, _| 0.0,
        );
        let r = DemoVolModel
            .slice(&doc, &at("2026-10-01", &[100.0]))
            .unwrap();
        assert!((r.points[0].vol - 0.2).abs() < 1e-12, "{}", r.points[0].vol);
        let r = DemoVolModel
            .slice(&doc, &at("2026-09-15", &[100.0]))
            .unwrap();
        assert!((r.points[0].vol - 0.2).abs() < 1e-12, "{}", r.points[0].vol);
        let r = DemoVolModel
            .slice(&doc, &at("2026-09-01", &[100.0]))
            .unwrap();
        assert!((r.points[0].vol - 0.2).abs() < 1e-12, "{}", r.points[0].vol);
    }

    #[test]
    fn a_node_at_or_below_minus_one_hundred_is_refused() {
        // Strike F(1+k) is non-positive at k <= -1, so the term is refused.
        for bad in [-100.0, -150.0] {
            let mut doc = flat(0.2);
            if let Some((_, Column::F64(nodes))) = doc.axes.iter_mut().find(|(n, _)| n == "node") {
                nodes[NODES.len()] = bad; // the second term's first node
            }
            let err = DemoVolModel
                .slice(&doc, &at("2026-10-16", &[100.0]))
                .unwrap_err();
            assert_eq!(
                err.0,
                "cvi_params document term 2027-04-16 has a node at or below -100%"
            );
        }
    }

    #[test]
    fn density_on_strikes_not_strictly_ascending_is_refused() {
        let doc = flat(0.2);
        for strikes in [vec![90.0, 110.0, 100.0], vec![90.0, 100.0, 100.0, 110.0]] {
            let err = DemoVolModel
                .slice(
                    &doc,
                    &SliceRequest {
                        expiry: date("2026-10-16"),
                        coordinate: Coordinate::Strike,
                        grid: Grid::At(strikes.clone()),
                        density: true,
                    },
                )
                .unwrap_err();
            assert_eq!(err.0, "density needs strictly ascending strikes");
            // Without density the same grid evaluates.
            let r = DemoVolModel
                .slice(&doc, &at("2026-10-16", &strikes))
                .unwrap();
            assert_eq!(r.points.len(), strikes.len());
        }
    }

    #[test]
    fn a_non_positive_or_nan_at_strike_is_refused() {
        let doc = flat(0.2);
        let err = DemoVolModel
            .slice(&doc, &at("2026-10-16", &[100.0, -5.0]))
            .unwrap_err();
        assert_eq!(err.0, "strike -5 is not positive");
        let err = DemoVolModel
            .slice(&doc, &at("2026-10-16", &[f64::NAN]))
            .unwrap_err();
        assert!(err.0.starts_with("strike NaN is not positive"), "{}", err.0);
    }

    #[test]
    fn moneyness_and_log_moneyness_are_strike_over_forward() {
        let doc = flat(0.2);
        for (coord, f) in [
            (
                Coordinate::Moneyness,
                (|k: f64| k / 100.0) as fn(f64) -> f64,
            ),
            (Coordinate::LogMoneyness, |k: f64| (k / 100.0).ln()),
        ] {
            let r = DemoVolModel
                .slice(
                    &doc,
                    &SliceRequest {
                        expiry: date("2026-10-16"),
                        coordinate: coord,
                        grid: Grid::At(vec![80.0, 100.0, 125.0]),
                        density: false,
                    },
                )
                .unwrap();
            for p in &r.points {
                assert!((p.x - f(p.strike)).abs() < 1e-12, "{coord:?} {p:?}");
            }
        }
    }

    #[test]
    fn delta_is_in_the_unit_interval_and_decreases_with_strike() {
        let doc = cvi_doc(
            "2026-09-01",
            &[
                ("2026-10-16", 100.0, 0.2, -0.8),
                ("2027-01-15", 100.0, 0.2, -0.8),
            ],
            |_, _| 0.0,
        );
        let r = DemoVolModel
            .slice(
                &doc,
                &SliceRequest {
                    expiry: date("2026-11-20"),
                    coordinate: Coordinate::Delta,
                    grid: Grid::Dense { n: 40, cover: None },
                    density: false,
                },
            )
            .unwrap();
        let xs: Vec<f64> = r.points.iter().map(|p| p.x).collect();
        assert!(xs.iter().all(|x| *x > 0.0 && *x < 1.0), "{xs:?}");
        assert!(xs.windows(2).all(|w| w[1] < w[0]), "{xs:?}");
    }

    #[test]
    fn a_map_places_chain_points_by_their_own_vols() {
        let req = MapRequest {
            expiry: date("2026-12-18"),
            as_of: date("2026-09-18"),
            forward: 100.0,
            coordinate: Coordinate::Delta,
            strikes: vec![90.0, 100.0, 110.0],
            vols: vec![0.30, 0.20, 0.15],
        };
        let xs = DemoVolModel.coordinates(&req).unwrap();
        let t = 91.0 / 365.0;
        for (i, (k, v)) in req.strikes.iter().zip(&req.vols).enumerate() {
            let expected = crate::black::call_delta(100.0, *k, *v, t);
            assert!((xs[i] - expected).abs() < 1e-12);
        }
        let money = DemoVolModel
            .coordinates(&MapRequest {
                coordinate: Coordinate::Moneyness,
                ..req.clone()
            })
            .unwrap();
        assert_eq!(money, vec![0.9, 1.0, 1.1]);
        let strike = DemoVolModel
            .coordinates(&MapRequest {
                coordinate: Coordinate::Strike,
                ..req.clone()
            })
            .unwrap();
        assert_eq!(strike, req.strikes);
    }

    #[test]
    fn a_map_with_mismatched_lengths_or_a_bad_forward_is_an_error() {
        let base = MapRequest {
            expiry: date("2026-12-18"),
            as_of: date("2026-09-18"),
            forward: 100.0,
            coordinate: Coordinate::Moneyness,
            strikes: vec![90.0, 100.0],
            vols: vec![0.3],
        };
        assert_eq!(
            DemoVolModel.coordinates(&base).unwrap_err().0,
            "map has 2 strikes and 1 vols"
        );
        let bad_forward = MapRequest {
            forward: 0.0,
            vols: vec![0.3, 0.2],
            ..base.clone()
        };
        assert_eq!(
            DemoVolModel.coordinates(&bad_forward).unwrap_err().0,
            "forward 0 is not positive"
        );
    }

    #[test]
    fn a_non_positive_strike_fails_the_job_naming_it() {
        let doc = flat(0.2);
        let err = DemoVolModel
            .slice(&doc, &at("2026-10-16", &[100.0, 0.0]))
            .unwrap_err();
        assert_eq!(err.0, "strike 0 is not positive");
        let req = MapRequest {
            expiry: date("2026-12-18"),
            as_of: date("2026-09-18"),
            forward: 100.0,
            coordinate: Coordinate::LogMoneyness,
            strikes: vec![100.0, -5.0],
            vols: vec![0.2, 0.2],
        };
        assert_eq!(
            DemoVolModel.coordinates(&req).unwrap_err().0,
            "strike -5 is not positive"
        );
    }

    #[test]
    fn a_flat_smiles_density_integrates_to_about_one_over_a_wide_grid() {
        let doc = flat(0.2);
        let strikes: Vec<f64> = (1..=3000).map(|i| i as f64 * 0.1).collect(); // 0.1 .. 300
        let r = DemoVolModel
            .slice(
                &doc,
                &SliceRequest {
                    expiry: date("2027-04-16"),
                    coordinate: Coordinate::Strike,
                    grid: Grid::At(strikes.clone()),
                    density: true,
                },
            )
            .unwrap();
        let d = r.density.unwrap();
        assert_eq!(d.len(), strikes.len() - 2, "interior points only");
        // Trapezoid over the interior points, x is the strike here.
        let mass: f64 = d
            .windows(2)
            .map(|w| 0.5 * (w[0].1 + w[1].1) * (w[1].0 - w[0].0))
            .sum();
        assert!((mass - 1.0).abs() < 0.02, "{mass}");
        assert!(
            d.iter().all(|(_, p)| *p >= -1e-9),
            "a flat smile has no negative lobe"
        );
        // The density sits at the interior points' x values.
        assert!((d[0].0 - r.points[1].x).abs() < 1e-12);
    }

    #[test]
    fn density_is_reported_in_the_requested_coordinate_and_is_unclamped() {
        // A wildly non-convex smile produces a negative lobe; it is returned as is.
        let doc = cvi_doc(
            "2026-09-01",
            &[
                ("2026-10-16", 100.0, 0.2, 0.0),
                ("2027-01-15", 100.0, 0.2, 0.0),
            ],
            |n, _| if n == 0.0 { -15.0 } else { 0.0 },
        );
        let r = DemoVolModel
            .slice(
                &doc,
                &SliceRequest {
                    expiry: date("2026-10-16"),
                    coordinate: Coordinate::Moneyness,
                    grid: Grid::Dense {
                        n: 200,
                        cover: None,
                    },
                    density: true,
                },
            )
            .unwrap();
        let d = r.density.unwrap();
        assert_eq!(d.len(), 198);
        assert!(d.iter().all(|(x, _)| *x > 0.7 && *x < 1.1), "moneyness x");
        assert!(d.iter().any(|(_, p)| *p < 0.0), "the negative lobe is kept");
    }

    #[test]
    fn a_flat_smiles_density_integrates_to_about_one_in_every_coordinate() {
        let doc = flat(0.2);
        let strikes: Vec<f64> = (1..=3000).map(|i| i as f64 * 0.1).collect();
        for coordinate in Coordinate::ALL {
            let r = DemoVolModel
                .slice(
                    &doc,
                    &SliceRequest {
                        expiry: date("2027-04-16"),
                        coordinate,
                        grid: Grid::At(strikes.clone()),
                        density: true,
                    },
                )
                .unwrap();
            let d = r.density.unwrap();
            // Trapezoid over |dx|, skipping the NaN where delta saturates.
            let mass: f64 = d
                .windows(2)
                .filter(|w| w[0].1.is_finite() && w[1].1.is_finite())
                .map(|w| 0.5 * (w[0].1 + w[1].1) * (w[1].0 - w[0].0).abs())
                .sum();
            assert!((mass - 1.0).abs() < 0.03, "{coordinate:?}: {mass}");
        }
    }
}
