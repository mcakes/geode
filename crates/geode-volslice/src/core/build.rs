//! The pure door between the tile's state and the vol door: which jobs
//! a repaint needs (`batch`) and what the answers paint (`model`). The
//! module never evaluates a vol; it names documents, expiries and
//! strikes and reads the results back by position.

use std::sync::Arc;
use std::time::Instant;

use chrono::NaiveDate;
use geode_core::document::DocumentRows;
use geode_core::query::QueryKey;
use geode_core::vol::{Coordinate, Grid, MapRequest, SliceRequest, VolJob, VolSliceParams};

use crate::core::model::{Kind, Loaded, Pair, State, StripRow};

/// Points per dense curve.
pub const GRID_N: usize = 200;

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
    /// A curve evaluated at the other side's strikes, for the difference.
    DiffCurve { kind: Kind, expiry: NaiveDate },
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub documents: Vec<Arc<DocumentRows>>,
    pub jobs: Vec<VolJob>,
    pub roles: Vec<Role>,
    pub coordinate: Coordinate,
    pub diff: Option<Pair>,
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
        let mut dense_job = [None; 3];
        for kind in [Kind::Cvi, Kind::Draft] {
            let Some(document) = doc_of[kind.index()] else {
                continue;
            };
            let trace = state.visible(loaded, kind);
            let minuend = state
                .diff
                .is_some_and(|p| p.minuend == kind && p.subtrahend.is_curve());
            if trace || minuend {
                dense_job[kind.index()] = Some(jobs.len());
                jobs.push(VolJob::Slice {
                    document,
                    request: slice(expiry, Grid::Dense(GRID_N), trace && state.density),
                });
                roles.push(Role::Curve {
                    kind,
                    expiry,
                    trace,
                });
            }
        }
        let chain = loaded.chain_at(expiry);
        if let Some(c) = chain {
            let trace = state.visible(loaded, Kind::Chain);
            let in_pair = state
                .diff
                .is_some_and(|p| p.minuend == Kind::Chain || p.subtrahend == Kind::Chain);
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
        let Some(pair) = state.diff else { continue };
        if pair.minuend.is_curve() && pair.subtrahend.is_curve() {
            let (Some(of), Some(document)) = (
                dense_job[pair.minuend.index()],
                doc_of[pair.subtrahend.index()],
            ) else {
                continue;
            };
            jobs.push(VolJob::Slice {
                document,
                request: slice(expiry, Grid::Job(of), false),
            });
            roles.push(Role::DiffCurve {
                kind: pair.subtrahend,
                expiry,
            });
        } else {
            let curve = if pair.minuend.is_curve() {
                pair.minuend
            } else {
                pair.subtrahend
            };
            let (Some(document), Some(c)) = (doc_of[curve.index()], chain) else {
                continue;
            };
            jobs.push(VolJob::Slice {
                document,
                request: slice(expiry, Grid::At(c.strikes.clone()), false),
            });
            roles.push(Role::DiffCurve {
                kind: curve,
                expiry,
            });
        }
    }
    Plan {
        documents,
        jobs,
        roles,
        coordinate: state.coordinate,
        diff: state.diff,
        active,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::tests::{TODAY, d, fixture};
    use crate::core::model::{Kind, Pair, State, strip};

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
            (1, &Grid::Dense(GRID_N), false)
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
            diff: Pair::new(Kind::Draft, Kind::Cvi),
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
                expiry: d("2026-10-16")
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

    #[test]
    fn a_hidden_minuend_still_gets_its_dense_job_without_a_trace() {
        let mut st = State {
            diff: Pair::new(Kind::Draft, Kind::Cvi),
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
            diff: Pair::new(Kind::Chain, Kind::Cvi),
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
            diff: Pair::new(Kind::Cvi, Kind::Chain),
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
}
