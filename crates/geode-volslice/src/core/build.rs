//! The pure door between the tile's state and the vol door: which jobs
//! a repaint needs (`batch`) and what the answers paint (`model`). The
//! module never evaluates a vol; it names documents, expiries and
//! strikes and reads the results back by position.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use chrono::NaiveDate;
use geode_chart::Axis;
use geode_chart::core::palette::HuePalette;
use geode_chart::xy::{SlotKind, Style, XAxis, XFormat, XyModel, XySlot, YFormat};
use geode_core::document::DocumentRows;
use geode_core::query::QueryKey;
use geode_core::vol::{
    Coordinate, Grid, MapRequest, SliceRequest, VolJob, VolResult, VolSliceOutcome, VolSliceParams,
};
use gpui::Hsla;

use crate::core::docs::ChainExpiry;
use crate::core::model::{Kind, Loaded, Pair, State, StripRow};

/// Points per dense curve over the CVI's node ladder widened to the listed
/// chain. The density is a second difference over these points, so its
/// smoothness is points per σ√t·F at the forward, where it has its mass.
/// The model chooses the spacing; the demo model packs points toward the
/// forward on the scale of σ√t, so that count holds however wide the
/// chain. A thousand is enough that even spread evenly over the widest
/// demo chain (under half the forward) they put some sixty across the
/// σ√t·F of a one-week, 20%-vol expiry (2.8% of the forward), past the
/// twenty-five that reads as a smooth hump; the vol curve's wings stay
/// fine too. A curve's path is decimated to its pixel columns, so the
/// count costs the chart nothing per frame; the model evaluates a Black
/// price per point, once per batch.
pub const GRID_N: usize = 1000;

/// What each job of a batch is for, by position.
#[derive(Debug, Clone, PartialEq)]
pub enum Role {
    /// A curve's dense slice; `trace: false` when only the difference needs it.
    Curve {
        kind: Kind,
        expiry: NaiveDate,
        trace: bool,
    },
    /// The chain's coordinates; `trace: false` when only the difference needs them.
    Chain { expiry: NaiveDate, trace: bool },
    /// A curve evaluated at another kind's strikes for a difference: the
    /// minuend curve's (`Grid::Job`) for two curves, the chain's (`Grid::At`)
    /// when `at` is the chain. Asked once per `(kind, expiry, at)` however
    /// many pairs read it.
    DiffCurve {
        kind: Kind,
        expiry: NaiveDate,
        at: Kind,
    },
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub documents: Vec<Arc<DocumentRows>>,
    pub jobs: Vec<VolJob>,
    pub roles: Vec<Role>,
    pub coordinate: Coordinate,
    /// The pairs shown, in their paint order.
    pub diffs: Vec<Pair>,
    /// Strip position (the palette index) and date of each active expiry.
    pub active: Vec<(usize, NaiveDate)>,
}

impl Plan {
    pub fn params(&self, key: QueryKey, tag: u64, submitted: Instant) -> VolSliceParams {
        VolSliceParams {
            key,
            tag,
            submitted,
            documents: self.documents.clone(),
            jobs: self.jobs.clone(),
        }
    }
}

/// The strike span a dense curve at a chain's expiry must reach: its
/// lowest and highest listed strike, so a curve is drawn at least as wide
/// as the quotes. Asked whether or not the chain is shown, so toggling the
/// chain does not move the curves' x extent. `None` without a chain, or
/// with no positive finite strike in it.
pub fn cover(chain: Option<&ChainExpiry>) -> Option<(f64, f64)> {
    let mut strikes = chain?
        .strikes
        .iter()
        .copied()
        .filter(|k| k.is_finite() && *k > 0.0);
    let first = strikes.next()?;
    Some(strikes.fold((first, first), |(lo, hi), k| (lo.min(k), hi.max(k))))
}

pub fn batch(state: &State, loaded: &Loaded, strip: &[StripRow]) -> Plan {
    let mut documents = Vec::new();
    let mut doc_of = [None; 3];
    for kind in [Kind::Cvi, Kind::Draft] {
        if let Some(doc) = loaded.document(kind) {
            doc_of[kind.index()] = Some(documents.len());
            documents.push(Arc::clone(doc));
        }
    }
    let active = state.active_in(strip);
    let (mut jobs, mut roles) = (Vec::new(), Vec::new());
    let slice = |expiry, grid, density| SliceRequest {
        expiry,
        coordinate: state.coordinate,
        grid,
        density,
    };
    for &(_, expiry) in &active {
        let chain = loaded.chain_at(expiry);
        let grid = Grid::Dense {
            n: GRID_N,
            cover: cover(chain),
        };
        let mut dense_job = [None; 3];
        for kind in [Kind::Cvi, Kind::Draft] {
            let Some(document) = doc_of[kind.index()] else {
                continue;
            };
            let trace = state.visible(loaded, kind);
            let minuend = state
                .diffs
                .iter()
                .any(|p| p.minuend == kind && p.subtrahend.is_curve());
            if trace || minuend {
                dense_job[kind.index()] = Some(jobs.len());
                jobs.push(VolJob::Slice {
                    document,
                    request: slice(expiry, grid.clone(), trace && state.density),
                });
                roles.push(Role::Curve {
                    kind,
                    expiry,
                    trace,
                });
            }
        }
        if let Some(c) = chain {
            let trace = state.visible(loaded, Kind::Chain);
            let in_pair = state.diffs.iter().any(|p| p.has_chain());
            if trace || in_pair {
                jobs.push(VolJob::Map(MapRequest {
                    expiry,
                    as_of: c.as_of,
                    forward: c.forward,
                    coordinate: state.coordinate,
                    strikes: c.strikes.clone(),
                    vols: c.mid.clone(),
                }));
                roles.push(Role::Chain { expiry, trace });
            }
        }
        // Each pair's curve at the other side's strikes. Two pairs reading
        // the same evaluation share its job.
        let mut asked: Vec<(Kind, Kind)> = Vec::new();
        for pair in &state.diffs {
            let (kind, at) = if pair.has_chain() {
                (pair.curve(), Kind::Chain)
            } else {
                (pair.subtrahend, pair.minuend)
            };
            let Some(document) = doc_of[kind.index()] else {
                continue;
            };
            if asked.contains(&(kind, at)) {
                continue;
            }
            let grid = match (at, chain) {
                (Kind::Chain, Some(c)) => Grid::At(c.strikes.clone()),
                (Kind::Chain, None) => continue,
                (minuend, _) => match dense_job[minuend.index()] {
                    Some(of) => Grid::Job(of),
                    None => continue,
                },
            };
            asked.push((kind, at));
            jobs.push(VolJob::Slice {
                document,
                request: slice(expiry, grid, false),
            });
            roles.push(Role::DiffCurve { kind, expiry, at });
        }
    }
    Plan {
        documents,
        jobs,
        roles,
        coordinate: state.coordinate,
        diffs: state.diffs.clone(),
        active,
    }
}

/// A curve-and-chain difference at the chain's strikes, from the curve's
/// vols there: the mid's difference as the point, and the bar the quote's
/// spread gives it. `curve − chain` runs from `curve − ask` to
/// `curve − bid`; `chain − curve` from `bid − curve` to `ask − curve`. A
/// one-sided quote's NaN side carries through, so the bar is the half the
/// quote has, as on the chain's own trace.
fn chain_diff(
    pair: Pair,
    curve: &[geode_core::vol::SlicePoint],
    chain: &ChainExpiry,
) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let at = |side: &[f64]| -> Vec<f64> {
        curve
            .iter()
            .zip(side)
            .map(|(p, q)| {
                if pair.minuend == Kind::Chain {
                    q - p.vol
                } else {
                    p.vol - q
                }
            })
            .collect()
    };
    let mid = at(&chain.mid);
    let (lo, hi) = if pair.minuend == Kind::Chain {
        (at(&chain.bid), at(&chain.ask))
    } else {
        (at(&chain.ask), at(&chain.bid))
    };
    (mid, lo, hi)
}

/// A density line's opacity against its curve's expiry color, so the pdf
/// reads as a companion of the smile rather than a second smile. Its fill
/// is the chart's fill opacity of this.
pub const DENSITY_ALPHA: f32 = 0.55;

/// The y formats in `Axis::ALL` order: vols on the left of either pane,
/// densities on the right.
const Y_FORMAT: [YFormat; 4] = [
    YFormat::Percent,
    YFormat::Plain,
    YFormat::Percent,
    YFormat::Plain,
];

/// A painted batch: the model, the notices of its failed jobs, and the
/// x extent of its visible slots.
pub struct Built {
    pub model: Arc<XyModel>,
    pub notices: Vec<String>,
    pub full: (f64, f64),
}

pub fn x_axis(coordinate: Coordinate) -> XAxis {
    match coordinate {
        Coordinate::Strike => XAxis {
            format: XFormat::Price,
            reversed: false,
        },
        Coordinate::Moneyness => XAxis {
            format: XFormat::Percent,
            reversed: false,
        },
        Coordinate::LogMoneyness => XAxis {
            format: XFormat::Fixed(2),
            reversed: false,
        },
        // Call delta falls as strike rises: reversed, puts stay on the left.
        Coordinate::Delta => XAxis {
            format: XFormat::Delta,
            reversed: true,
        },
    }
}

/// The narrowest x span a view may show in a coordinate. A strike view
/// keeps at least one chain strike gap, so a zoom cannot leave a single
/// quote alone between the axis ends.
pub fn min_span(coordinate: Coordinate, loaded: &Loaded) -> f64 {
    match coordinate {
        Coordinate::Strike => {
            let mut gap = f64::INFINITY;
            for c in &loaded.chain {
                let mut strikes: Vec<f64> = c
                    .strikes
                    .iter()
                    .copied()
                    .filter(|k| k.is_finite())
                    .collect();
                strikes.sort_by(f64::total_cmp);
                for w in strikes.windows(2) {
                    let g = w[1] - w[0];
                    if g > 0.0 && g < gap {
                        gap = g;
                    }
                }
            }
            if gap.is_finite() { gap } else { 1.0 }
        }
        Coordinate::Moneyness | Coordinate::LogMoneyness => 0.01,
        Coordinate::Delta => 0.02,
    }
}

/// Widen a range narrower than `min` to `min` about its centre: a one-x
/// extent (a single strike) is otherwise a zero-width axis.
pub fn padded(full: (f64, f64), min: f64) -> (f64, f64) {
    let (lo, hi) = full;
    if hi - lo >= min {
        return full;
    }
    let centre = (lo + hi) / 2.0;
    (centre - min / 2.0, centre + min / 2.0)
}

/// The same slots at another split, under a new version: the element's
/// caches tell models apart by version alone.
pub fn with_split(model: &XyModel, split: f32, version: u64) -> Arc<XyModel> {
    XyModel::new(version, model.x, model.y_format, split, model.slots.clone())
}

fn failure(role: &Role, e: &str) -> String {
    match role {
        Role::Curve { kind, expiry, .. } | Role::DiffCurve { kind, expiry, .. } => {
            format!("no {} curve at {expiry}: {e}", kind.label())
        }
        Role::Chain { expiry, .. } => format!("no chain coordinates at {expiry}: {e}"),
    }
}

/// The color a difference paints in at the expiry whose palette index is
/// `pos`: the expiry's color, except that a pair of the draft with the
/// chain takes the expiry's companion. Pairs at one expiry are told apart
/// by their mark first (two curves are a line, anything with the chain
/// points with bars) and, of the two pairs with the chain, by this color.
pub fn diff_color(pair: Pair, palette: &HuePalette, pos: usize) -> Hsla {
    if pair.has_chain() && pair.curve() == Kind::Draft {
        palette.companion(pos)
    } else {
        palette.color(pos)
    }
}

/// Read a batch's answers by position into the slots they paint. An
/// outcome shorter than its plan (a cancelled batch) answers `None`:
/// indexing it would pair results with the wrong roles, and a newer
/// batch supersedes it anyway.
///
/// Colors come from `palette` by strip position: each expiry's own hue,
/// the published curve and the draft in its color (the draft dashed), the
/// chain in its companion, a density at [`DENSITY_ALPHA`] of its curve's.
/// Differences follow every trace, expiry by expiry and each expiry's
/// pairs in their order ([`diff_color`]).
pub fn model(
    plan: &Plan,
    outcome: &VolSliceOutcome,
    loaded: &Loaded,
    palette: &HuePalette,
    split: f32,
    version: u64,
) -> Option<Built> {
    let n = plan.jobs.len();
    if outcome.results.len() < n || plan.roles.len() < n {
        return None;
    }
    let results = &outcome.results[..n];
    let slice = |i: usize| match &results[i] {
        Ok(VolResult::Slice(s)) => Some(s),
        _ => None,
    };
    let map = |i: usize| match &results[i] {
        Ok(VolResult::Map(xs)) => Some(xs),
        _ => None,
    };

    let mut notices: Vec<String> = Vec::new();
    let mut slots: Vec<XySlot> = Vec::new();
    // Slot numbers count from one in push order.
    fn push(
        slots: &mut Vec<XySlot>,
        label: String,
        color: Hsla,
        axis: Axis,
        style: Style,
        kind: SlotKind,
    ) {
        slots.push(XySlot {
            number: (slots.len() + 1) as u16,
            label: label.into(),
            color,
            axis,
            visible: true,
            style,
            kind,
        });
    }
    // A difference reads its expiry's dense curve and chain jobs by
    // lookup, not by adjacency, so it cannot pair with another expiry's.
    let mut curve_at: HashMap<(Kind, NaiveDate), usize> = HashMap::new();
    let mut chain_of: HashMap<NaiveDate, usize> = HashMap::new();
    let mut diff_at: HashMap<(Kind, NaiveDate, Kind), usize> = HashMap::new();
    for (i, role) in plan.roles[..n].iter().enumerate() {
        match *role {
            Role::Curve { kind, expiry, .. } => {
                curve_at.insert((kind, expiry), i);
            }
            Role::Chain { expiry, .. } => {
                chain_of.insert(expiry, i);
            }
            Role::DiffCurve { kind, expiry, at } => {
                diff_at.insert((kind, expiry, at), i);
            }
        }
    }

    for (i, role) in plan.roles[..n].iter().enumerate() {
        let expiry = match role {
            Role::Curve { expiry, .. }
            | Role::Chain { expiry, .. }
            | Role::DiffCurve { expiry, .. } => *expiry,
        };
        if let Err(e) = &results[i] {
            // A difference whose strikes come from a failed job fails
            // because that job did, and that job's own notice says why:
            // a second notice would name job indices a trader never sees.
            if let VolJob::Slice { request, .. } = &plan.jobs[i]
                && let Grid::Job(of) = request.grid
                && results.get(of).is_some_and(Result::is_err)
            {
                continue;
            }
            let notice = failure(role, e);
            if !notices.contains(&notice) {
                notices.push(notice);
            }
            continue;
        }
        let pos = plan
            .active
            .iter()
            .find(|(_, e)| *e == expiry)
            .map_or(0, |(pos, _)| *pos);
        let color = palette.color(pos);
        match *role {
            Role::Curve { kind, trace, .. } => {
                let Some(s) = slice(i) else { continue };
                if !trace {
                    continue;
                }
                let style = if kind == Kind::Draft {
                    Style::Dashed
                } else {
                    Style::Solid
                };
                push(
                    &mut slots,
                    format!("{} {expiry}", kind.label()),
                    color,
                    Axis::Left,
                    style,
                    SlotKind::Line {
                        xs: s.points.iter().map(|p| p.x).collect(),
                        ys: s.points.iter().map(|p| p.vol).collect(),
                        fill: false,
                    },
                );
                if let Some(density) = &s.density {
                    push(
                        &mut slots,
                        format!("{} density {expiry}", kind.label()),
                        Hsla {
                            a: DENSITY_ALPHA,
                            ..color
                        },
                        Axis::Right,
                        style,
                        // Shaded down to zero, a negative lobe up to it.
                        SlotKind::Line {
                            xs: density.iter().map(|(x, _)| *x).collect(),
                            ys: density.iter().map(|(_, pdf)| *pdf).collect(),
                            fill: true,
                        },
                    );
                }
            }
            Role::Chain { trace, .. } => {
                let (Some(xs), Some(c)) = (map(i), loaded.chain_at(expiry)) else {
                    continue;
                };
                if !trace {
                    continue;
                }
                // Each coordinate pairs with its quote by position: a
                // length mismatch would paint quotes at other strikes' x.
                // One chain job per expiry, so the notice is already unique.
                if xs.len() != c.mid.len() {
                    notices.push(failure(
                        role,
                        &format!("{} coordinates for {} quotes", xs.len(), c.mid.len()),
                    ));
                    continue;
                }
                // One-sided quotes carry a NaN bid or ask: the chart paints
                // the half bar the quote has.
                push(
                    &mut slots,
                    format!("chain {expiry}"),
                    palette.companion(pos),
                    Axis::Left,
                    Style::Solid,
                    SlotKind::Points {
                        xs: xs.clone(),
                        mid: c.mid.clone(),
                        lo: c.bid.clone(),
                        hi: c.ask.clone(),
                    },
                );
            }
            // Painted below, once every trace is.
            Role::DiffCurve { .. } => {}
        }
    }

    for &(pos, expiry) in &plan.active {
        for &pair in &plan.diffs {
            let label = format!("{} {expiry}", pair.label());
            let color = diff_color(pair, palette, pos);
            if !pair.has_chain() {
                // Equal strikes: the subtrahend was evaluated at the
                // minuend's, so the points pair by position.
                let at = diff_at.get(&(pair.subtrahend, expiry, pair.minuend));
                let minuend = curve_at.get(&(pair.minuend, expiry));
                let (Some(other), Some(minuend)) = (
                    at.copied().and_then(slice),
                    minuend.copied().and_then(slice),
                ) else {
                    continue;
                };
                if minuend.points.len() != other.points.len() {
                    continue;
                }
                push(
                    &mut slots,
                    label,
                    color,
                    Axis::BottomLeft,
                    Style::Solid,
                    SlotKind::Line {
                        xs: minuend.points.iter().map(|p| p.x).collect(),
                        ys: minuend
                            .points
                            .iter()
                            .zip(&other.points)
                            .map(|(a, b)| a.vol - b.vol)
                            .collect(),
                        fill: false,
                    },
                );
                continue;
            }
            // The curve was evaluated at the chain strikes and sits at the
            // chain's x; the quote's bid and ask carry over as the bar.
            let curve = pair.curve();
            let (Some(other), Some(xs), Some(c)) = (
                diff_at
                    .get(&(curve, expiry, Kind::Chain))
                    .copied()
                    .and_then(slice),
                chain_of.get(&expiry).copied().and_then(map),
                loaded.chain_at(expiry),
            ) else {
                continue;
            };
            let n = other.points.len();
            if xs.len() != n || [&c.mid, &c.bid, &c.ask].iter().any(|v| v.len() != n) {
                continue;
            }
            let (mid, lo, hi) = chain_diff(pair, &other.points, c);
            push(
                &mut slots,
                label,
                color,
                Axis::BottomLeft,
                Style::Solid,
                SlotKind::Points {
                    xs: xs.clone(),
                    mid,
                    lo,
                    hi,
                },
            );
        }
    }

    // One cause behind every job (no model, a full queue) is said once,
    // as the outcome words it, not once per expiry and kind.
    if let Some(Err(first)) = results.first()
        && results.iter().all(|r| r.as_ref().err() == Some(first))
    {
        notices = vec![first.clone()];
    }
    // A pair naming a kind with nothing loaded (restored, or kept while a
    // draft left) asks no difference job: without this it paints nothing,
    // silently. Said per pair, worded as `:diff` refuses such a pair.
    for pair in &plan.diffs {
        if let Some(k) = [pair.minuend, pair.subtrahend]
            .into_iter()
            .find(|k| !loaded.has(*k))
        {
            notices.push(format!(
                "diff {}: {} is not loaded",
                pair.label(),
                k.label()
            ));
        }
    }

    let model = XyModel::new(version, x_axis(plan.coordinate), Y_FORMAT, split, slots);
    let full = model.full();
    Some(Built {
        model,
        notices,
        full,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::tests::{TODAY, d, fixture};
    use crate::core::model::{Kind, Pair, State, strip};
    use geode_data::vol::{VolConfig, evaluate};

    fn plan(state: &mut State) -> Plan {
        let l = fixture();
        let s = strip(&l, d(TODAY));
        state.reconcile(&s);
        batch(state, &l, &s)
    }

    #[test]
    fn the_front_expiry_asks_both_curves_dense() {
        let p = plan(&mut State::default());
        assert_eq!(p.documents.len(), 2, "cvi then draft");
        assert_eq!(
            p.roles,
            vec![
                Role::Curve {
                    kind: Kind::Cvi,
                    expiry: d("2026-10-16"),
                    trace: true
                },
                Role::Curve {
                    kind: Kind::Draft,
                    expiry: d("2026-10-16"),
                    trace: true
                },
            ],
            "no chain at the front term"
        );
        let VolJob::Slice { document, request } = &p.jobs[1] else {
            panic!()
        };
        assert_eq!(
            (*document, &request.grid, request.density),
            (
                1,
                &Grid::Dense {
                    n: GRID_N,
                    cover: None
                },
                false
            )
        );
    }

    /// The dense grids of a chain expiry reach its lowest and highest
    /// listed strike, the chain shown or not, so hiding the chain does not
    /// move the curves' x extent.
    #[test]
    fn a_chain_expirys_curves_cover_its_listed_strikes_shown_or_not() {
        let dense = |p: &Plan| -> Vec<Grid> {
            p.roles
                .iter()
                .zip(&p.jobs)
                .filter_map(|(r, j)| match (r, j) {
                    (Role::Curve { .. }, VolJob::Slice { request, .. }) => {
                        Some(request.grid.clone())
                    }
                    _ => None,
                })
                .collect()
        };
        let want = Grid::Dense {
            n: GRID_N,
            cover: Some((85.0, 115.0)),
        };
        let mut st = State {
            active: Some([d("2026-11-20")].into()),
            ..State::default()
        };
        assert_eq!(dense(&plan(&mut st)), [want.clone(), want.clone()]);
        st.toggle_kind(Kind::Chain);
        let p = plan(&mut st);
        assert!(!p.roles.iter().any(|r| matches!(r, Role::Chain { .. })));
        assert_eq!(dense(&p), [want.clone(), want]);
    }

    #[test]
    fn a_cover_is_the_chains_positive_finite_strike_span() {
        use crate::core::model::tests::chain;
        let mut c = chain("2026-11-20");
        assert_eq!(cover(Some(&c)), Some((85.0, 115.0)));
        c.strikes = vec![110.0, f64::NAN, 90.0, -5.0, 0.0, 120.0, f64::INFINITY];
        assert_eq!(cover(Some(&c)), Some((90.0, 120.0)), "in any order");
        c.strikes = vec![f64::NAN, 0.0];
        assert_eq!(cover(Some(&c)), None);
        assert_eq!(cover(None), None);
    }

    /// A one-week expiry beside a chain about as wide as the widest demo
    /// chain (46% of the forward): the density's steepest step between
    /// neighbours stays a small fraction of its peak, which takes some 25
    /// points or more per σ√t·F.
    #[test]
    fn a_one_week_density_across_a_wide_chain_is_smooth() {
        use crate::core::model::tests::{chain, cvi};
        let mut c = chain("2026-10-08");
        c.strikes = (0..=46).map(|i| 70.0 + i as f64).collect();
        for side in [&mut c.bid, &mut c.mid, &mut c.ask] {
            *side = vec![0.2; 47];
        }
        let l = Loaded {
            cvi: Some(cvi(&["2026-10-08", "2026-12-18"], None)),
            draft: None,
            chain: vec![c],
        };
        let s = strip(&l, d(TODAY));
        let mut st = State {
            active: Some([d("2026-10-08")].into()),
            density: true,
            ..State::default()
        };
        st.reconcile(&s);
        let p = batch(&st, &l, &s);
        let b = model(&p, &answer(&p), &l, &palette(), st.split, 1).unwrap();
        assert!(b.notices.is_empty(), "{:?}", b.notices);
        let density = b
            .model
            .slots
            .iter()
            .find(|s| s.label.contains("density"))
            .unwrap();
        let SlotKind::Line { xs, ys, .. } = &density.kind else {
            panic!()
        };
        assert!(
            xs[0] < 0.71 && xs[xs.len() - 1] > 1.15,
            "across the chain: {}..{}",
            xs[0],
            xs[xs.len() - 1]
        );
        let peak = ys.iter().copied().fold(f64::MIN, f64::max);
        let steepest = ys
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f64::max);
        assert!(
            steepest / peak < 0.025,
            "steepest step {} of the peak",
            steepest / peak
        );
    }

    #[test]
    fn a_chain_expiry_between_terms_asks_curves_and_a_map() {
        let mut st = State {
            active: Some([d("2026-11-20")].into()),
            ..State::default()
        };
        let p = plan(&mut st);
        assert_eq!(p.roles.len(), 3);
        let VolJob::Map(m) = &p.jobs[2] else { panic!() };
        assert_eq!(m.as_of, d("2026-10-02"));
        assert_eq!(m.vols, vec![0.20; 7], "mid vols");
    }

    #[test]
    fn hidden_kinds_are_omitted_and_density_rides_visible_curves() {
        let mut st = State {
            active: Some([d("2026-11-20")].into()),
            density: true,
            ..State::default()
        };
        st.toggle_kind(Kind::Draft);
        st.toggle_kind(Kind::Chain);
        let p = plan(&mut st);
        assert_eq!(
            p.roles,
            vec![Role::Curve {
                kind: Kind::Cvi,
                expiry: d("2026-11-20"),
                trace: true
            }]
        );
        let VolJob::Slice { request, .. } = &p.jobs[0] else {
            panic!()
        };
        assert!(request.density);
    }

    #[test]
    fn curve_minus_curve_evaluates_the_subtrahend_at_the_minuends_strikes() {
        let mut st = State {
            diffs: Pair::new(Kind::Draft, Kind::Cvi).into_iter().collect(),
            ..State::default()
        };
        let p = plan(&mut st);
        let j = p
            .roles
            .iter()
            .position(|r| {
                matches!(
                    r,
                    Role::Curve {
                        kind: Kind::Draft,
                        ..
                    }
                )
            })
            .unwrap();
        let (i, _) = p
            .roles
            .iter()
            .enumerate()
            .find(|(_, r)| matches!(r, Role::DiffCurve { .. }))
            .unwrap();
        assert_eq!(
            p.roles[i],
            Role::DiffCurve {
                kind: Kind::Cvi,
                expiry: d("2026-10-16"),
                at: Kind::Draft,
            }
        );
        let VolJob::Slice { document, request } = &p.jobs[i] else {
            panic!()
        };
        assert_eq!(
            (*document, &request.grid, request.density),
            (0, &Grid::Job(j), false)
        );
        assert!(j < i, "the grid names an earlier job");
    }

    /// With two active expiries each difference evaluates at its own
    /// expiry's minuend strikes: a grid naming the other expiry's curve
    /// would pair strikes of one term with vols of another. A hidden
    /// subtrahend asks no trace of its own but still gets its difference
    /// job: hiding a kind to read the difference is a use.
    #[test]
    fn each_expirys_difference_names_its_own_minuend_and_a_hidden_subtrahend_still_gets_one() {
        let expiries = [d("2026-10-16"), d("2026-12-18")];
        let mut st = State {
            active: Some(expiries.into()),
            diffs: Pair::new(Kind::Draft, Kind::Cvi).into_iter().collect(),
            ..State::default()
        };
        let p = plan(&mut st);
        let minuend_of = |expiry| {
            p.roles
                .iter()
                .position(|r| {
                    *r == Role::Curve {
                        kind: Kind::Draft,
                        expiry,
                        trace: true,
                    }
                })
                .unwrap()
        };
        let diffs: Vec<(NaiveDate, Grid)> = p
            .roles
            .iter()
            .zip(&p.jobs)
            .filter_map(|(r, j)| match (r, j) {
                (Role::DiffCurve { expiry, .. }, VolJob::Slice { request, .. }) => {
                    Some((*expiry, request.grid.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            diffs,
            expiries
                .iter()
                .map(|e| (*e, Grid::Job(minuend_of(*e))))
                .collect::<Vec<_>>()
        );

        st.toggle_kind(Kind::Cvi);
        let p = plan(&mut st);
        assert!(
            !p.roles.iter().any(|r| matches!(
                r,
                Role::Curve {
                    kind: Kind::Cvi,
                    ..
                }
            )),
            "the hidden subtrahend paints no curve: {:?}",
            p.roles
        );
        let hidden: Vec<NaiveDate> = p
            .roles
            .iter()
            .filter_map(|r| match r {
                Role::DiffCurve {
                    kind: Kind::Cvi,
                    expiry,
                    ..
                } => Some(*expiry),
                _ => None,
            })
            .collect();
        assert_eq!(
            hidden, expiries,
            "but each expiry still gets its difference"
        );
    }

    #[test]
    fn a_hidden_minuend_still_gets_its_dense_job_without_a_trace() {
        let mut st = State {
            diffs: Pair::new(Kind::Draft, Kind::Cvi).into_iter().collect(),
            ..State::default()
        };
        st.toggle_kind(Kind::Draft);
        let p = plan(&mut st);
        assert!(p.roles.contains(&Role::Curve {
            kind: Kind::Draft,
            expiry: d("2026-10-16"),
            trace: false
        }));
    }

    #[test]
    fn curve_minus_chain_evaluates_the_curve_at_the_chain_strikes() {
        let mut st = State {
            active: Some([d("2026-11-20")].into()),
            diffs: Pair::new(Kind::Chain, Kind::Cvi).into_iter().collect(),
            ..State::default()
        };
        st.toggle_kind(Kind::Chain);
        let p = plan(&mut st);
        assert!(
            p.roles.contains(&Role::Chain {
                expiry: d("2026-11-20"),
                trace: false
            }),
            "the diff needs its x"
        );
        let i = p
            .roles
            .iter()
            .position(|r| matches!(r, Role::DiffCurve { .. }))
            .unwrap();
        let VolJob::Slice { request, .. } = &p.jobs[i] else {
            panic!()
        };
        assert_eq!(request.grid, Grid::At(fixture().chain[0].strikes.clone()));
    }

    #[test]
    fn a_pair_whose_kind_is_absent_at_an_expiry_adds_nothing_there() {
        let mut st = State {
            diffs: Pair::new(Kind::Cvi, Kind::Chain).into_iter().collect(),
            ..State::default()
        };
        let p = plan(&mut st); // front term has no chain
        assert!(!p.roles.iter().any(|r| matches!(r, Role::DiffCurve { .. })));
    }

    #[test]
    fn params_carry_the_documents_and_jobs_under_the_key_and_tag() {
        let p = plan(&mut State::default());
        let params = p.params(QueryKey(9), 4, Instant::now());
        assert_eq!(
            (params.key, params.tag, params.jobs.len()),
            (QueryKey(9), 4, 2)
        );
    }

    fn palette() -> HuePalette {
        let c = |l| gpui::hsla(0.0, 0.0, l, 1.0);
        HuePalette::from_theme([c(0.1), c(0.2), c(0.3), c(0.4), c(0.5)], c(1.0), c(0.0))
    }

    fn answer(plan: &Plan) -> VolSliceOutcome {
        let config = VolConfig::with(Arc::new(geode_pricing::DemoVolModel));
        let params = plan.params(QueryKey(1), 1, Instant::now());
        VolSliceOutcome {
            key: params.key,
            tag: 1,
            submitted: params.submitted,
            results: evaluate(&config, &params),
        }
    }

    fn built(state: &mut State) -> Built {
        let l = fixture();
        let s = strip(&l, d(TODAY));
        state.reconcile(&s);
        let p = batch(state, &l, &s);
        model(&p, &answer(&p), &l, &palette(), state.split, 7).unwrap()
    }

    #[test]
    fn curves_and_chains_become_slots_by_kind_style_and_expiry_color() {
        let mut st = State {
            active: Some([d("2026-10-16"), d("2026-11-20")].into()),
            ..State::default()
        };
        let b = built(&mut st);
        let labels: Vec<_> = b.model.slots.iter().map(|s| s.label.to_string()).collect();
        assert_eq!(
            labels,
            [
                "cvi 2026-10-16",
                "cvi draft 2026-10-16",
                "cvi 2026-11-20",
                "cvi draft 2026-11-20",
                "chain 2026-11-20"
            ]
        );
        assert_eq!(b.model.slots[1].style, Style::Dashed);
        assert_eq!(b.model.slots[0].color, palette().color(0));
        assert_eq!(
            b.model.slots[2].color,
            palette().color(1),
            "the strip position, not the active index"
        );
        assert_eq!(b.model.slots[3].color, palette().color(1), "the draft too");
        assert_eq!(
            b.model.slots[4].color,
            palette().companion(1),
            "the chain is its expiry's companion"
        );
        let SlotKind::Points { lo, hi, .. } = &b.model.slots[4].kind else {
            panic!()
        };
        assert_eq!((lo[0], hi[0]), (0.19, 0.21));
        assert_eq!(b.model.version, 7);
        assert!(b.notices.is_empty());
    }

    #[test]
    fn densities_paint_filled_on_the_right_axis_and_curves_unfilled() {
        let mut st = State {
            density: true,
            ..State::default()
        };
        let b = built(&mut st);
        let fills = |s: &XySlot| matches!(s.kind, SlotKind::Line { fill: true, .. });
        let (densities, rest): (Vec<_>, Vec<_>) = b
            .model
            .slots
            .iter()
            .partition(|s| s.label.contains("density"));
        assert_eq!(densities.len(), 2, "cvi and draft");
        assert!(
            densities.iter().all(|s| s.axis == Axis::Right && fills(s)),
            "{densities:?}"
        );
        assert!(!rest.iter().any(|s| fills(s)), "{rest:?}");
    }

    #[test]
    fn a_draft_atm_edit_is_the_difference_at_equal_strikes() {
        // The draft lifts atm by 0.01 at 2026-12-18 alone: at that term the
        // diff is 0.01 at every strike (the stand-in's atm lifts every point).
        let mut st = State {
            active: Some([d("2026-12-18")].into()),
            diffs: Pair::new(Kind::Draft, Kind::Cvi).into_iter().collect(),
            ..State::default()
        };
        let b = built(&mut st);
        let diff = b
            .model
            .slots
            .iter()
            .find(|s| s.axis == Axis::BottomLeft)
            .unwrap();
        let SlotKind::Line { ys, .. } = &diff.kind else {
            panic!("curve-curve is a line")
        };
        assert_eq!(ys.len(), GRID_N);
        assert!(ys.iter().all(|y| (y - 0.01).abs() < 1e-9), "{:?}", &ys[..3]);
    }

    #[test]
    fn a_curve_minus_chain_difference_sits_at_the_chain_x() {
        let mut st = State {
            active: Some([d("2026-11-20")].into()),
            diffs: Pair::new(Kind::Chain, Kind::Cvi).into_iter().collect(),
            ..State::default()
        };
        let b = built(&mut st);
        let chain = b
            .model
            .slots
            .iter()
            .find(|s| s.label.starts_with("chain"))
            .unwrap();
        let diff = b
            .model
            .slots
            .iter()
            .find(|s| s.axis == Axis::BottomLeft)
            .unwrap();
        assert_eq!(diff.xs(), chain.xs());
        let SlotKind::Points { mid, lo, hi, .. } = &diff.kind else {
            panic!()
        };
        // `chain − cvi`: the quote's bid and ask less the curve, a bar a
        // spread wide about the mid's difference.
        for k in 0..mid.len() {
            assert!((lo[k] - (mid[k] - 0.01)).abs() < 1e-12, "{k}: {lo:?}");
            assert!((hi[k] - (mid[k] + 0.01)).abs() < 1e-12, "{k}: {hi:?}");
        }
    }

    #[test]
    fn swapping_a_curve_chain_pair_negates_the_difference() {
        let at = |pair: Option<Pair>| {
            let mut st = State {
                active: Some([d("2026-11-20")].into()),
                diffs: pair.into_iter().collect(),
                ..State::default()
            };
            let b = built(&mut st);
            let diff = b.model.slots.iter().find(|s| s.axis == Axis::BottomLeft);
            let SlotKind::Points { mid, .. } = &diff.unwrap().kind else {
                panic!()
            };
            mid.clone()
        };
        let curve_first = at(Pair::new(Kind::Cvi, Kind::Chain));
        let chain_first = at(Pair::new(Kind::Chain, Kind::Cvi));
        assert!(
            curve_first.iter().any(|y| y.abs() > 1e-6),
            "{curve_first:?}"
        );
        let negated: Vec<f64> = chain_first.iter().map(|y| -y).collect();
        assert_eq!(curve_first, negated);
    }

    /// `cvi − chain` is the curve's vol at a chain strike less that
    /// strike's mid, in value and sign: swapping alone cannot tell a
    /// difference painted upside down in both orders.
    #[test]
    fn a_curve_minus_chain_difference_is_the_curve_vol_less_the_mid() {
        let l = fixture();
        let s = strip(&l, d(TODAY));
        let mut st = State {
            active: Some([d("2026-11-20")].into()),
            diffs: Pair::new(Kind::Cvi, Kind::Chain).into_iter().collect(),
            ..State::default()
        };
        st.reconcile(&s);
        let p = batch(&st, &l, &s);
        let o = answer(&p);
        let b = model(&p, &o, &l, &palette(), st.split, 1).unwrap();
        let at = p
            .roles
            .iter()
            .position(|r| matches!(r, Role::DiffCurve { .. }))
            .unwrap();
        let Ok(VolResult::Slice(curve)) = &o.results[at] else {
            panic!("the curve at the chain strikes")
        };
        let mid = &l.chain_at(d("2026-11-20")).unwrap().mid;
        let diff = b.model.slots.iter().find(|s| s.axis == Axis::BottomLeft);
        let SlotKind::Points {
            mid: ys, lo, hi, ..
        } = &diff.unwrap().kind
        else {
            panic!()
        };
        let k = 0;
        let expected = curve.points[k].vol - mid[k];
        assert!(expected.abs() > 1e-6, "the fixture separates them");
        assert_eq!(ys[k], expected);
        let c = l.chain_at(d("2026-11-20")).unwrap();
        assert_eq!(
            (lo[k], hi[k]),
            (
                curve.points[k].vol - c.ask[k],
                curve.points[k].vol - c.bid[k]
            ),
            "the curve less the ask, up to the curve less the bid"
        );
    }

    /// A one-sided quote's missing side carries into the difference as a
    /// NaN end, so the bar is the half the quote has.
    #[test]
    fn a_one_sided_quote_gives_its_difference_a_half_bar() {
        let mut l = fixture();
        l.chain[0].bid[1] = f64::NAN;
        let s = strip(&l, d(TODAY));
        let mut st = State {
            active: Some([d("2026-11-20")].into()),
            diffs: vec![Pair::new(Kind::Cvi, Kind::Chain).unwrap()],
            ..State::default()
        };
        st.reconcile(&s);
        let p = batch(&st, &l, &s);
        let b = model(&p, &answer(&p), &l, &palette(), st.split, 1).unwrap();
        let diff = b.model.slots.iter().find(|s| s.axis == Axis::BottomLeft);
        let SlotKind::Points { mid, lo, hi, .. } = &diff.unwrap().kind else {
            panic!()
        };
        assert!(mid[1].is_finite() && lo[1].is_finite(), "{mid:?} {lo:?}");
        assert!(hi[1].is_nan(), "curve − bid has no bid: {hi:?}");
    }

    /// Two pairs at once: the jobs they share (dense curves, the chain's
    /// map, an evaluation at the chain's strikes) are asked once per
    /// expiry; each pair paints its own slot per expiry, in the order the
    /// pairs were turned on.
    #[test]
    fn several_pairs_share_their_jobs_and_paint_in_turn_on_order() {
        let pairs = vec![
            Pair::new(Kind::Draft, Kind::Chain).unwrap(),
            Pair::new(Kind::Cvi, Kind::Draft).unwrap(),
            Pair::new(Kind::Cvi, Kind::Chain).unwrap(),
        ];
        let mut st = State {
            active: Some([d("2026-11-20"), d("2026-12-18")].into()),
            diffs: pairs.clone(),
            ..State::default()
        };
        let p = plan(&mut st);
        let count = |want: &dyn Fn(&Role) -> bool| p.roles.iter().filter(|r| want(r)).count();
        assert_eq!(
            count(&|r| matches!(r, Role::Curve { .. })),
            4,
            "{:?}",
            p.roles
        );
        assert_eq!(
            count(&|r| matches!(r, Role::Chain { .. })),
            1,
            "one chain expiry"
        );
        let diff_roles: Vec<&Role> = p
            .roles
            .iter()
            .filter(|r| matches!(r, Role::DiffCurve { .. }))
            .collect();
        assert_eq!(
            diff_roles,
            [
                &Role::DiffCurve {
                    kind: Kind::Draft,
                    expiry: d("2026-11-20"),
                    at: Kind::Chain
                },
                &Role::DiffCurve {
                    kind: Kind::Draft,
                    expiry: d("2026-11-20"),
                    at: Kind::Cvi
                },
                &Role::DiffCurve {
                    kind: Kind::Cvi,
                    expiry: d("2026-11-20"),
                    at: Kind::Chain
                },
                &Role::DiffCurve {
                    kind: Kind::Draft,
                    expiry: d("2026-12-18"),
                    at: Kind::Cvi
                },
            ],
            "no chain at 2026-12-18, so only the curves' pair there"
        );

        let b = built(&mut st);
        let lower: Vec<(String, Hsla, bool)> = b
            .model
            .slots
            .iter()
            .filter(|s| s.axis == Axis::BottomLeft)
            .map(|s| {
                (
                    s.label.to_string(),
                    s.color,
                    matches!(s.kind, SlotKind::Points { .. }),
                )
            })
            .collect();
        let (pos1, pos2) = (1, 2);
        assert_eq!(
            lower,
            [
                (
                    format!("{} 2026-11-20", pairs[0].label()),
                    palette().companion(pos1),
                    true
                ),
                (
                    format!("{} 2026-11-20", pairs[1].label()),
                    palette().color(pos1),
                    false
                ),
                (
                    format!("{} 2026-11-20", pairs[2].label()),
                    palette().color(pos1),
                    true
                ),
                (
                    format!("{} 2026-12-18", pairs[1].label()),
                    palette().color(pos2),
                    false
                ),
            ]
        );
        assert!(b.notices.is_empty(), "{:?}", b.notices);
    }

    /// A pair and its reverse read the same evaluation at the chain's
    /// strikes. The tile never holds both, but a state that did asks it
    /// once and paints both pairs from it.
    #[test]
    fn a_pair_and_its_reverse_share_one_evaluation() {
        let p = Pair::new(Kind::Cvi, Kind::Chain).unwrap();
        let mut st = State {
            active: Some([d("2026-11-20")].into()),
            diffs: vec![p, p.reverse()],
            ..State::default()
        };
        let plan = plan(&mut st);
        let evaluations = plan
            .roles
            .iter()
            .filter(|r| matches!(r, Role::DiffCurve { .. }))
            .count();
        assert_eq!(evaluations, 1, "{:?}", plan.roles);
        let b = built(&mut st);
        let lower = b.model.slots.iter().filter(|s| s.axis == Axis::BottomLeft);
        assert_eq!(lower.count(), 2);
    }

    #[test]
    fn failed_jobs_become_deduplicated_notices() {
        // 2027-06-18 is past the last term: both curves refuse there, and
        // the cvi's evaluation at the chain strikes for the difference
        // refuses with the very same words as its dense curve.
        let mut st = State {
            active: Some([d("2026-10-16"), d("2027-06-18")].into()),
            diffs: Pair::new(Kind::Cvi, Kind::Chain).into_iter().collect(),
            ..State::default()
        };
        let b = built(&mut st);
        assert_eq!(b.notices.len(), 2, "one per kind: {:?}", b.notices);
        assert!(
            b.notices[0].starts_with("no cvi curve at 2027-06-18: expiry 2027-06-18 is outside"),
            "{:?}",
            b.notices
        );
        assert!(
            b.model
                .slots
                .iter()
                .any(|s| s.label.as_ref() == "chain 2027-06-18"),
            "the chain still paints"
        );
    }

    /// Past the last term both dense curves refuse, and the difference
    /// reading the minuend's strikes fails because its source did: the two
    /// curves' notices say why, and the difference adds none.
    #[test]
    fn a_difference_whose_strikes_failed_adds_no_notice() {
        let mut st = State {
            active: Some([d("2027-06-18")].into()),
            diffs: Pair::new(Kind::Draft, Kind::Cvi).into_iter().collect(),
            ..State::default()
        };
        let b = built(&mut st);
        assert_eq!(b.notices.len(), 2, "{:?}", b.notices);
        assert!(
            b.notices.iter().all(|n| !n.contains("job")),
            "{:?}",
            b.notices
        );
    }

    /// A chain whose coordinates do not match its quotes one for one is
    /// skipped with a notice rather than painted at other strikes' x.
    #[test]
    fn a_chain_whose_coordinates_miscount_its_quotes_is_skipped() {
        let l = fixture();
        let s = strip(&l, d(TODAY));
        let mut st = State {
            active: Some([d("2026-11-20")].into()),
            ..State::default()
        };
        st.reconcile(&s);
        let p = batch(&st, &l, &s);
        let mut o = answer(&p);
        let at = p
            .roles
            .iter()
            .position(|r| matches!(r, Role::Chain { .. }))
            .unwrap();
        let Ok(VolResult::Map(xs)) = &mut o.results[at] else {
            panic!("the chain's coordinates")
        };
        xs.pop();
        let b = model(&p, &o, &l, &palette(), st.split, 1).unwrap();
        assert!(
            !b.model.slots.iter().any(|s| s.label.starts_with("chain")),
            "no chain slot"
        );
        assert_eq!(
            b.notices,
            vec!["no chain coordinates at 2026-11-20: 6 coordinates for 7 quotes".to_string()]
        );
    }

    /// A pair naming a kind with nothing loaded says so: it asks no
    /// difference job, so otherwise nothing would paint and nothing say why.
    #[test]
    fn a_pair_naming_an_unloaded_kind_says_so() {
        let mut l = fixture();
        l.draft = None;
        let s = strip(&l, d(TODAY));
        let mut st = State {
            diffs: Pair::new(Kind::Draft, Kind::Cvi).into_iter().collect(),
            ..State::default()
        };
        st.reconcile(&s);
        let p = batch(&st, &l, &s);
        let b = model(&p, &answer(&p), &l, &palette(), st.split, 1).unwrap();
        assert_eq!(
            b.notices,
            vec!["diff cvi draft \u{2212} cvi: cvi draft is not loaded".to_string()]
        );
        st.diffs = vec![
            Pair::new(Kind::Cvi, Kind::Chain).unwrap(),
            Pair::new(Kind::Chain, Kind::Draft).unwrap(),
        ];
        let p = batch(&st, &l, &s);
        let b = model(&p, &answer(&p), &l, &palette(), st.split, 1).unwrap();
        assert_eq!(
            b.notices,
            vec!["diff chain \u{2212} cvi draft: cvi draft is not loaded".to_string()],
            "said for the pair that names it, not its neighbour"
        );
        let p = batch(&st, &fixture(), &s);
        let b = model(&p, &answer(&p), &fixture(), &palette(), st.split, 1).unwrap();
        assert!(b.notices.is_empty(), "both loaded: {:?}", b.notices);
    }

    #[test]
    fn one_message_for_every_job_is_said_once() {
        let l = fixture();
        let s = strip(&l, d(TODAY));
        let mut st = State::default();
        st.reconcile(&s);
        let p = batch(&st, &l, &s);
        let params = p.params(QueryKey(1), 1, Instant::now());
        let o = VolSliceOutcome {
            key: params.key,
            tag: 1,
            submitted: params.submitted,
            results: evaluate(&VolConfig::missing("demo"), &params),
        };
        let b = model(&p, &o, &l, &palette(), 0.7, 1).unwrap();
        assert_eq!(
            b.notices,
            vec!["vol model \"demo\" is not built into this binary".to_string()]
        );
    }

    #[test]
    fn a_short_outcome_is_superseded_not_indexed() {
        let l = fixture();
        let s = strip(&l, d(TODAY));
        let mut st = State::default();
        st.reconcile(&s);
        let p = batch(&st, &l, &s);
        let mut o = answer(&p);
        o.results.pop();
        assert!(model(&p, &o, &l, &palette(), 0.7, 1).is_none());
    }

    #[test]
    fn the_view_is_rebuilt_with_the_coordinates_min_span() {
        let l = fixture();
        assert_eq!(
            min_span(Coordinate::Strike, &l),
            5.0,
            "the chain's strike gap"
        );
        assert_eq!(min_span(Coordinate::Delta, &l), 0.02);
        let (lo, hi) = padded((1.0, 1.0), 0.02);
        assert!(
            (lo - 0.99).abs() < 1e-12 && (hi - 1.01).abs() < 1e-12,
            "a one-x range is padded: {lo} {hi}"
        );
        assert_eq!(padded((0.8, 1.2), 0.02), (0.8, 1.2));
        let a = x_axis(Coordinate::Delta);
        assert!(a.reversed && a.format == XFormat::Delta);
    }

    #[test]
    fn a_split_change_is_a_new_version_with_the_same_slots() {
        let b = built(&mut State::default());
        let m = with_split(&b.model, 0.5, 8);
        assert_eq!(
            (m.version, m.split, m.slots.len()),
            (8, 0.5, b.model.slots.len())
        );
    }
}
