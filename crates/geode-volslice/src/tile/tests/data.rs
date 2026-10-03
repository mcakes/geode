//! The data flow: two document queries under one tag, the board, the vol
//! batch and the model swap, visibility, close and restore. Every test
//! drives the shell's doors: deliveries through `TileContent::deliver`,
//! keys through the keymap, frame changes through the frame's
//! test-support entry points.

use super::*;
use crate::core::build::batch;
use crate::core::docs::{CHAIN, CVI, ChainExpiry};
use crate::core::model::tests::{TERMS, TODAY, chain, cvi, d};
use crate::core::model::{Loaded, State, strip};
use geode_core::attribution::{Attribution, ScopeSemantics};
use geode_core::document::{Column, DocumentRows, Value};
use geode_core::link::{BoardEntry, DraftMark, Emission, Group, Membership, underlying_scope};
use geode_core::query::{DocumentParams, QueryKey, QueryOutcome};
use geode_core::scope::{DimensionSelection, Scope};
use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
use geode_core::vol::{VolJob, VolSliceOutcome, VolSliceParams};
use geode_data::vol::{VolConfig, evaluate};
use geode_shell::module::Delivery;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

const EMITTER: TileId = TileId(9);
const KEY: QueryKey = QueryKey(TILE);

/// A tile whose frame handle is bound to it, as the shell binds every
/// occupant's: following a link group changes what it reads.
fn open_bound(
    cx: &mut gpui::TestAppContext,
    restored: Option<toml::Table>,
) -> (Harness, gpui::VisualTestContext) {
    open_framed(cx, restored, |frame| {
        FrameRef::for_tile(frame, WorkspaceIx::FIRST, TileId(TILE))
    })
}

/// A tile restored from `restored`, its handle bound to no tile.
fn open_on(
    cx: &mut gpui::TestAppContext,
    restored: toml::Table,
) -> (Harness, gpui::VisualTestContext) {
    open_framed(cx, Some(restored), |frame| {
        FrameRef::new(frame, WorkspaceIx::FIRST)
    })
}

/// What the factory's `launch_state` writes for an underlying.
fn launched_on(u: &str) -> toml::Table {
    let mut t = toml::Table::new();
    t.insert("underlying".into(), toml::Value::String(u.into()));
    t
}

fn meta(name: &str) -> ColumnMeta {
    ColumnMeta {
        name: name.into(),
        attribution_by_depth: vec![Attribution::Additive],
        scope_semantics: ScopeSemantics::Direct,
        summable: false,
        mixed_flag: None,
    }
}

fn leak(s: String) -> Option<&'static str> {
    Some(&*Box::leak(s.into_boxed_str()))
}

fn f64s(c: &Column) -> Vec<Option<f64>> {
    match c {
        Column::F64(v) => v.iter().map(|x| Some(*x)).collect(),
        _ => panic!("not an f64 column"),
    }
}

/// The snapshot the data tier answers a `cvi_params` read with: the
/// document's columns, one row per (term, node).
fn cvi_snapshot(doc: &DocumentRows) -> Snapshot {
    let n = doc.rows();
    let mut cols = vec![(
        meta("underlying_ref"),
        TestColumn::Str(vec![leak(doc.key[0].clone()); n]),
    )];
    for (name, c) in &doc.axes {
        let col = match c {
            Column::Date(v) => TestColumn::Date(v.iter().map(|x| Some(*x)).collect()),
            c => TestColumn::F64(f64s(c)),
        };
        cols.push((meta(name), col));
    }
    for (name, c) in &doc.values {
        cols.push((meta(name), TestColumn::F64(f64s(c))));
    }
    for (name, v) in &doc.attributes {
        let col = match v {
            Value::Date(x) => TestColumn::Date(vec![Some(*x); n]),
            Value::F64(x) => TestColumn::F64(vec![Some(*x); n]),
            other => panic!("unexpected attribute {other:?}"),
        };
        cols.push((meta(name), col));
    }
    Snapshot::for_tests(cols, 0)
}

/// The snapshot an `option_chain` prefix read answers with: every
/// expiry's quotes, contiguous, under the dataset's column names.
fn chain_snapshot(chains: &[ChainExpiry]) -> Snapshot {
    let (mut exp, mut strike, mut bid, mut mid, mut ask, mut fwd, mut qt) =
        (vec![], vec![], vec![], vec![], vec![], vec![], vec![]);
    for c in chains {
        for i in 0..c.strikes.len() {
            exp.push(leak(c.expiry.to_string()));
            strike.push(Some(c.strikes[i]));
            bid.push(Some(c.bid[i]));
            mid.push(Some(c.mid[i]));
            ask.push(Some(c.ask[i]));
            fwd.push(Some(c.forward));
            qt.push(leak(format!("{}T15:30:00Z", c.as_of)));
        }
    }
    let n = exp.len();
    Snapshot::for_tests(
        vec![
            (
                meta("underlying_ref"),
                TestColumn::Str(vec![Some("SPX.Z"); n]),
            ),
            (meta("expiry"), TestColumn::Str(exp)),
            (meta("strike"), TestColumn::F64(strike)),
            (meta("bid_vol"), TestColumn::F64(bid)),
            (meta("ask_vol"), TestColumn::F64(ask)),
            (meta("mid_vol"), TestColumn::F64(mid)),
            (meta("forward"), TestColumn::F64(fwd)),
            (meta("quote_time"), TestColumn::Str(qt)),
        ],
        0,
    )
}

/// The fixture's published documents: three CVI terms, and two chain
/// expiries (one between terms, one past the last term).
fn published() -> (Arc<DocumentRows>, Vec<ChainExpiry>) {
    (
        cvi(&TERMS, None),
        vec![chain("2026-11-20"), chain("2027-06-18")],
    )
}

fn docs(reqs: &[Request]) -> Vec<&DocumentParams> {
    reqs.iter()
        .filter_map(|r| match r {
            Request::Document(p) => Some(p),
            _ => None,
        })
        .collect()
}

fn vols(reqs: &[Request]) -> Vec<&VolSliceParams> {
    reqs.iter()
        .filter_map(|r| match r {
            Request::VolSlices(p) => Some(p),
            _ => None,
        })
        .collect()
}

fn scope_of(u: &str) -> Emission {
    Emission {
        scope: Some(underlying_scope(u)),
        board: Vec::new(),
    }
}

fn draft_of(u: &str, rows: &Arc<DocumentRows>, mark: DraftMark) -> Emission {
    Emission {
        scope: Some(underlying_scope(u)),
        board: vec![BoardEntry {
            dataset: CVI.into(),
            key: vec![u.into()],
            rows: Arc::clone(rows),
            mark,
        }],
    }
}

impl Harness {
    /// Pin "today" to the fixture's date, then show the tile as the
    /// shell does.
    fn show(&self, vcx: &mut gpui::VisualTestContext) {
        self.tile.update(vcx, |t, _| t.today_pin = Some(d(TODAY)));
        vcx.update(|_, cx| self.content.set_visible(true, cx));
        vcx.run_until_parked();
    }
    fn deliver(&self, vcx: &mut gpui::VisualTestContext, delivery: Delivery) {
        vcx.update(|window, cx| self.content.deliver(delivery, window, cx));
        vcx.run_until_parked();
    }
    fn answer_doc(&self, vcx: &mut gpui::VisualTestContext, tag: u64, snapshot: Snapshot) {
        self.deliver(
            vcx,
            Delivery::Query(QueryOutcome {
                key: KEY,
                tag,
                snapshot: Ok(Arc::new(snapshot)),
                submitted: Instant::now(),
            }),
        );
    }
    /// Answer a vol batch as the vol worker would, over the stand-in.
    fn answer_vol(&self, vcx: &mut gpui::VisualTestContext, params: &VolSliceParams) {
        let config = VolConfig::with(Arc::new(geode_pricing::DemoVolModel));
        self.deliver(
            vcx,
            Delivery::VolSlices(VolSliceOutcome {
                key: params.key,
                tag: params.tag,
                submitted: params.submitted,
                results: evaluate(&config, params),
            }),
        );
    }
    /// Answer the outstanding cvi request, then the chain request it leads
    /// to. Returns what the chain's answer made the tile submit.
    fn answer_documents(
        &self,
        vcx: &mut gpui::VisualTestContext,
        doc: &DocumentRows,
        chains: &[ChainExpiry],
    ) -> Vec<Request> {
        let reqs = self.requests();
        let asked = docs(&reqs);
        let cvi_req = asked.last().expect("a cvi request is out");
        assert_eq!(cvi_req.dataset, CVI);
        self.answer_doc(vcx, cvi_req.tag, cvi_snapshot(doc));
        let reqs = self.requests();
        let chain_req = docs(&reqs);
        assert_eq!(chain_req.len(), 1, "the chain follows the cvi");
        assert_eq!(chain_req[0].dataset, CHAIN);
        self.answer_doc(vcx, chain_req[0].tag, chain_snapshot(chains));
        self.requests()
    }
    fn notices(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.tile.read_with(vcx, |t, _| t.notices())
    }
    fn labels(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.tile.read_with(vcx, |t, _| {
            t.model()
                .slots
                .iter()
                .map(|s| s.label.to_string())
                .collect()
        })
    }
    fn version(&self, vcx: &gpui::VisualTestContext) -> u64 {
        self.tile.read_with(vcx, |t, _| t.model().version)
    }
    fn draft_label(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile.read_with(vcx, |t, _| t.draft_label())
    }
    fn link(&self, vcx: &mut gpui::VisualTestContext, tile: TileId, membership: Membership) {
        self.frame.update(vcx, |f, cx| {
            f.link_for_test(tile, membership);
            cx.notify();
        });
        vcx.run_until_parked();
    }
    fn post(&self, vcx: &mut gpui::VisualTestContext, emission: Emission) {
        self.frame.update(vcx, |f, cx| {
            f.post_for_test(EMITTER, emission);
            cx.notify();
        });
        vcx.run_until_parked();
    }
    /// The emitter emits into A, then the tile follows A.
    fn follow_a(&self, vcx: &mut gpui::VisualTestContext) {
        self.link(
            vcx,
            EMITTER,
            Membership {
                follow: None,
                emit: Some(Group::A),
            },
        );
        self.link(
            vcx,
            TileId(TILE),
            Membership {
                follow: Some(Group::A),
                emit: None,
            },
        );
    }
    fn open_flip(&self, vcx: &mut gpui::VisualTestContext) {
        self.frame.update(vcx, |f, cx| {
            f.shared_mut().open_flip([KEY], Instant::now());
            cx.notify();
        });
        vcx.run_until_parked();
    }
    fn barrier_open(&self, vcx: &gpui::VisualTestContext) -> bool {
        self.frame.read_with(vcx, |f, _| f.barrier_open())
    }
}

/// The batch the fixture's first paint needs: the front expiry, fronted
/// on a fresh strip.
fn expected_first_batch(loaded: &Loaded) -> Vec<VolJob> {
    let s = strip(loaded, d(TODAY));
    let mut st = State::default();
    st.reconcile(&s);
    batch(&st, loaded, &s).jobs
}

/// The query pool keeps one request per key, so the two documents go out
/// in sequence: the chain is asked only once the cvi has answered, under
/// the same key and tag, and the vol batch only once both have.
#[gpui::test]
fn first_load_asks_cvi_then_the_chain_under_one_tag(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_on(cx, launched_on("SPX.Z"));
    h.show(&mut vcx);
    let reqs = h.requests();
    let first = docs(&reqs);
    assert_eq!(first.len(), 1, "one request at a time: {reqs:?}");
    assert_eq!(first[0].dataset, CVI);
    assert_eq!(first[0].document_key, vec!["SPX.Z".to_string()]);
    assert_eq!(first[0].key, KEY);
    let tag = first[0].tag;
    let (doc, chains) = published();
    h.answer_doc(&mut vcx, tag, cvi_snapshot(&doc));
    let reqs = h.requests();
    let second = docs(&reqs);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].dataset, CHAIN);
    assert_eq!(second[0].document_key, vec!["SPX.Z".to_string()]);
    assert_eq!((second[0].key, second[0].tag), (KEY, tag), "the same tag");
    assert!(vols(&reqs).is_empty(), "no batch before the chain lands");
    h.answer_doc(&mut vcx, tag, chain_snapshot(&chains));
    let reqs = h.requests();
    let batches = vols(&reqs);
    assert_eq!(batches.len(), 1);
    let loaded = Loaded {
        cvi: Some(doc),
        draft: None,
        chain: chains,
    };
    assert_eq!(batches[0].jobs, expected_first_batch(&loaded));
    assert_eq!(batches[0].documents.len(), 1, "the published cvi alone");
    let params = batches[0].clone();
    h.answer_vol(&mut vcx, &params);
    assert_eq!(h.labels(&vcx), vec!["cvi 2026-10-16".to_string()]);
    assert!(h.notices(&vcx).is_empty(), "{:?}", h.notices(&vcx));
}

/// The pair is one answer to the flip barrier: the cvi's delivery leaves
/// the tile awaited, the chain's releases it.
#[gpui::test]
fn the_barrier_sees_one_arrival_for_both_documents(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_on(cx, launched_on("SPX.Z"));
    h.show(&mut vcx);
    let reqs = h.requests();
    let tag = docs(&reqs)[0].tag;
    h.open_flip(&mut vcx);
    assert!(h.barrier_open(&vcx), "the query in flight is the answer");
    assert!(h.requests().is_empty(), "an open flip asks nothing new");
    let (doc, chains) = published();
    h.answer_doc(&mut vcx, tag, cvi_snapshot(&doc));
    assert!(h.barrier_open(&vcx), "the cvi alone is not an arrival");
    let _ = h.requests();
    h.answer_doc(&mut vcx, tag, chain_snapshot(&chains));
    assert!(!h.barrier_open(&vcx), "the pair arrives once and releases");
    assert_eq!(vols(&h.requests()).len(), 1, "and applies at once");
}

/// A chain submission the data tier refuses fails the whole fetch: the
/// refusal is the notice, and the failure is the tile's arrival.
#[gpui::test]
fn a_refused_chain_submission_fails_the_fetch_with_the_refusal_worded(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_on(cx, launched_on("SPX.Z"));
    h.show(&mut vcx);
    let reqs = h.requests();
    let tag = docs(&reqs)[0].tag;
    h.open_flip(&mut vcx);
    h.data.fill_for_tests();
    let (doc, _) = published();
    h.answer_doc(&mut vcx, tag, cvi_snapshot(&doc));
    assert_eq!(
        h.notices(&vcx),
        vec!["document request refused: the data service is busy".to_string()]
    );
    assert!(!h.barrier_open(&vcx), "a failed fetch still arrives");
    let reqs = h.requests();
    assert!(docs(&reqs).is_empty() && vols(&reqs).is_empty());
}

/// Two batches in flight: the first's late answer is dropped, and the
/// model's version moves once, for the answer applied. The two batches ask
/// the same jobs, so only the tag can tell the older answer apart.
#[gpui::test]
fn an_older_vol_tag_is_dropped(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, None);
    h.show(&mut vcx);
    h.follow_a(&mut vcx);
    let draft = cvi(&TERMS, Some(("2026-10-16", 0.01)));
    h.post(&mut vcx, draft_of("SPX.Z", &draft, DraftMark::Editing));
    let (doc, chains) = published();
    let reqs = h.answer_documents(&mut vcx, &doc, &chains);
    let first = vols(&reqs).last().map(|p| (*p).clone()).expect("a batch");
    // A board bump: the same draft falling behind, a second batch.
    h.post(&mut vcx, draft_of("SPX.Z", &draft, DraftMark::Behind));
    let reqs = h.requests();
    let second = vols(&reqs)[0].clone();
    assert_ne!(first.tag, second.tag);
    assert_eq!(first.jobs, second.jobs, "the same question twice");
    let before = h.version(&vcx);
    h.answer_vol(&mut vcx, &first);
    assert_eq!(h.version(&vcx), before, "the older answer is dropped");
    h.answer_vol(&mut vcx, &second);
    assert_eq!(
        h.version(&vcx),
        before + 1,
        "one version per applied answer"
    );
    let labels = h.labels(&vcx);
    assert!(
        labels.iter().any(|l| l == "cvi draft 2026-10-16"),
        "{labels:?}"
    );
}

/// An active expiry past the last CVI term: the curve job fails, the
/// chain still paints, and the failure is the footer notice.
#[gpui::test]
fn a_refused_expiry_paints_chain_only_with_the_notice(cx: &mut gpui::TestAppContext) {
    let mut t = launched_on("SPX.Z");
    t.insert(
        "expiries".into(),
        toml::Value::Array(vec![toml::Value::String("2027-06-18".into())]),
    );
    let (h, mut vcx) = open_on(cx, t);
    h.show(&mut vcx);
    let (doc, chains) = published();
    let reqs = h.answer_documents(&mut vcx, &doc, &chains);
    let params = vols(&reqs)[0].clone();
    h.answer_vol(&mut vcx, &params);
    assert_eq!(h.labels(&vcx), vec!["chain 2027-06-18".to_string()]);
    let notices = h.notices(&vcx);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].starts_with(
            "no cvi curve at 2027-06-18: expiry 2027-06-18 is outside the document's terms"
        ),
        "{notices:?}"
    );
}

/// A draft posted on the followed group's board goes into the next batch
/// as a second document, without a document requery; a change of mark
/// alone is a board change too.
#[gpui::test]
fn a_board_bump_issues_a_batch_with_the_draft(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, None);
    h.show(&mut vcx);
    h.follow_a(&mut vcx);
    h.post(&mut vcx, scope_of("SPX.Z"));
    let (doc, chains) = published();
    let reqs = h.answer_documents(&mut vcx, &doc, &chains);
    assert_eq!(vols(&reqs)[0].documents.len(), 1);
    let draft = cvi(&TERMS, Some(("2026-10-16", 0.01)));
    h.post(&mut vcx, draft_of("SPX.Z", &draft, DraftMark::Editing));
    let reqs = h.requests();
    assert!(docs(&reqs).is_empty(), "a board change is not a requery");
    let batch = vols(&reqs);
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].documents.len(), 2, "cvi, then the draft");
    assert!(Arc::ptr_eq(&batch[0].documents[1], &draft));
    assert_eq!(h.draft_label(&vcx), Some("cvi draft".to_string()));
    h.post(&mut vcx, draft_of("SPX.Z", &draft, DraftMark::Behind));
    assert_eq!(vols(&h.requests()).len(), 1, "the mark is a board change");
    assert_eq!(
        h.draft_label(&vcx),
        Some("cvi draft \u{00b7} behind".to_string())
    );
}

/// A followed group that names no single underlying leaves nothing to
/// ask: the notice says so, and `u` points at the group instead of
/// opening a picker.
#[gpui::test]
fn following_a_group_with_no_single_underlying_paints_the_notice_and_refuses_u(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_bound(cx, None);
    h.show(&mut vcx);
    assert_eq!(h.notices(&vcx), vec!["no underlying".to_string()]);
    h.follow_a(&mut vcx);
    assert_eq!(h.notices(&vcx), vec!["no underlying in A".to_string()]);
    let two = Scope {
        dimensions: vec![DimensionSelection {
            column: geode_core::link::UNDERLYING.into(),
            values: vec!["SPX.Z".into(), "NDX.Z".into()],
        }],
        ..Scope::default()
    };
    h.post(
        &mut vcx,
        Emission {
            scope: Some(two),
            board: Vec::new(),
        },
    );
    assert_eq!(h.notices(&vcx), vec!["no underlying in A".to_string()]);
    vcx.simulate_keystrokes("u");
    assert!(
        h.notices(&vcx)
            .contains(&"following A \u{2014} set the underlying there".to_string()),
        "{:?}",
        h.notices(&vcx)
    );
    assert!(h.requests().is_empty(), "nothing asked, and no picker");
}

/// A group's scope change moves the follower's own scope generation, so
/// it requeries under the new underlying and its arrival is the one the
/// flip awaits.
#[gpui::test]
fn a_group_scope_change_flips_the_follower(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, None);
    h.show(&mut vcx);
    h.follow_a(&mut vcx);
    h.post(&mut vcx, scope_of("SPX.Z"));
    let (doc, chains) = published();
    let reqs = h.answer_documents(&mut vcx, &doc, &chains);
    assert_eq!(vols(&reqs).len(), 1);
    // The group's scope moves and the shell enrolls the follower under
    // its own reading, as `extend_flip` does.
    let bound = FrameRef::for_tile(h.frame.clone(), WorkspaceIx::FIRST, TileId(TILE));
    bound.update(&mut vcx, |f, cx| {
        f.post_for_test(EMITTER, scope_of("NDX.Z"));
        f.open_flip([KEY], Instant::now());
        cx.notify();
    });
    vcx.run_until_parked();
    let reqs = h.requests();
    let asked = docs(&reqs);
    assert_eq!(asked.len(), 1, "{reqs:?}");
    assert_eq!(asked[0].document_key, vec!["NDX.Z".to_string()]);
    let tag = asked[0].tag;
    assert!(h.barrier_open(&vcx));
    h.answer_doc(&mut vcx, tag, cvi_snapshot(&doc));
    assert!(h.barrier_open(&vcx));
    let _ = h.requests();
    h.answer_doc(&mut vcx, tag, chain_snapshot(&chains));
    assert!(!h.barrier_open(&vcx), "the follower's arrival released it");
}

/// A group's scope change that leaves its underlying where it was asks
/// the documents nothing: they depend only on the underlying, the as-of
/// and their publications. The follower answers the flip at once instead
/// of holding every other tile behind two document reads.
#[gpui::test]
fn a_group_scope_change_keeping_the_underlying_releases_the_flip_unasked(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_bound(cx, None);
    h.show(&mut vcx);
    h.follow_a(&mut vcx);
    h.post(&mut vcx, scope_of("SPX.Z"));
    let (doc, chains) = published();
    let reqs = h.answer_documents(&mut vcx, &doc, &chains);
    let params = vols(&reqs)[0].clone();
    h.answer_vol(&mut vcx, &params);
    let painted = h.labels(&vcx);
    assert!(!painted.is_empty());
    let mut wider = underlying_scope("SPX.Z");
    wider.dimensions.push(DimensionSelection {
        column: "book".into(),
        values: vec!["B1".into()],
    });
    let bound = FrameRef::for_tile(h.frame.clone(), WorkspaceIx::FIRST, TileId(TILE));
    bound.update(&mut vcx, |f, cx| {
        f.post_for_test(
            EMITTER,
            Emission {
                scope: Some(wider),
                board: Vec::new(),
            },
        );
        f.open_flip([KEY], Instant::now());
        cx.notify();
    });
    vcx.run_until_parked();
    let reqs = h.requests();
    assert!(docs(&reqs).is_empty(), "nothing asked: {reqs:?}");
    assert!(!h.barrier_open(&vcx), "the follower answered the flip");
    assert_eq!(h.labels(&vcx), painted, "the picture stays");
    // A later as-of change is still a new question.
    h.frame.update(&mut vcx, |f, cx| {
        f.shared_mut()
            .set_as_of(geode_core::query::AsOf::At(chrono::Utc::now()));
        cx.notify();
    });
    vcx.run_until_parked();
    assert_eq!(docs(&h.requests()).len(), 1, "the as-of asks again");
}

/// Leaving a group returns the tile to its own underlying: the draft and
/// the board watch go, and one requery goes out. The old documents are
/// not repainted without the draft in the meantime.
#[gpui::test]
fn leaving_a_group_drops_the_draft_and_returns_to_the_own_underlying(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_bound(cx, Some(launched_on("NDX.Z")));
    h.show(&mut vcx);
    let (doc, chains) = published();
    let reqs = h.answer_documents(&mut vcx, &doc, &chains);
    assert_eq!(vols(&reqs).len(), 1);
    // Follow A, whose scope is SPX.Z with a draft on its board.
    let draft = cvi(&TERMS, Some(("2026-10-16", 0.01)));
    h.link(
        &mut vcx,
        EMITTER,
        Membership {
            follow: None,
            emit: Some(Group::A),
        },
    );
    h.post(&mut vcx, draft_of("SPX.Z", &draft, DraftMark::Editing));
    assert!(h.requests().is_empty(), "nobody follows A yet");
    h.link(
        &mut vcx,
        TileId(TILE),
        Membership {
            follow: Some(Group::A),
            emit: None,
        },
    );
    // SPX.Z's draft is on the board while NDX.Z's documents are still the
    // ones loaded: it waits for its own underlying's.
    let reqs = h.requests();
    assert!(vols(&reqs).is_empty(), "no draft over NDX.Z: {reqs:?}");
    let tag = docs(&reqs)[0].tag;
    h.answer_doc(&mut vcx, tag, cvi_snapshot(&doc));
    let _ = h.requests();
    h.answer_doc(&mut vcx, tag, chain_snapshot(&chains));
    let reqs = h.requests();
    let painted = vols(&reqs).last().map(|p| (*p).clone()).unwrap();
    assert_eq!(
        painted.documents.len(),
        2,
        "the group's draft rides the batch"
    );
    assert_eq!(h.draft_label(&vcx), Some("cvi draft".to_string()));
    h.answer_vol(&mut vcx, &painted);
    let has_draft = |labels: Vec<String>| labels.iter().any(|l| l.starts_with("cvi draft"));
    assert!(has_draft(h.labels(&vcx)), "{:?}", h.labels(&vcx));
    // A board bump leaves a draft batch in flight across the leave.
    h.post(&mut vcx, draft_of("SPX.Z", &draft, DraftMark::Behind));
    let reqs = h.requests();
    let in_flight = vols(&reqs).last().map(|p| (*p).clone()).unwrap();
    assert_eq!(in_flight.documents.len(), 2);

    h.link(&mut vcx, TileId(TILE), Membership::default());
    let reqs = h.requests();
    let asked = docs(&reqs);
    assert_eq!(asked.len(), 1, "{reqs:?}");
    assert_eq!(asked[0].document_key, vec!["NDX.Z".to_string()]);
    assert!(vols(&reqs).is_empty(), "{reqs:?}");
    assert_eq!(h.draft_label(&vcx), None, "the draft left with the group");
    assert!(
        !has_draft(h.labels(&vcx)),
        "no draft trace under no draft chip: {:?}",
        h.labels(&vcx)
    );
    h.answer_vol(&mut vcx, &in_flight);
    assert!(
        !has_draft(h.labels(&vcx)),
        "the pre-leave batch cannot land: {:?}",
        h.labels(&vcx)
    );
    let again = cvi(&TERMS, Some(("2026-10-16", 0.02)));
    h.post(&mut vcx, draft_of("SPX.Z", &again, DraftMark::Editing));
    assert!(
        h.requests().is_empty(),
        "the board watch left with the group"
    );
}

/// Following a group whose scope was never written and leaving it moves no
/// frame version: the change of group alone sends the tile back to its own
/// underlying, which it asks for at once.
#[gpui::test]
fn leaving_a_never_written_group_asks_for_the_own_underlying(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, Some(launched_on("NDX.Z")));
    h.show(&mut vcx);
    let (doc, chains) = published();
    h.answer_documents(&mut vcx, &doc, &chains);
    h.link(
        &mut vcx,
        TileId(TILE),
        Membership {
            follow: Some(Group::A),
            emit: None,
        },
    );
    assert_eq!(h.notices(&vcx), vec!["no underlying in A".to_string()]);
    assert!(docs(&h.requests()).is_empty(), "A names nothing to ask");
    h.link(&mut vcx, TileId(TILE), Membership::default());
    let reqs = h.requests();
    let asked = docs(&reqs);
    assert_eq!(asked.len(), 1, "{reqs:?}");
    assert_eq!(asked[0].document_key, vec!["NDX.Z".to_string()]);
}

/// A restored tile asks once on its first show (a reshow with nothing
/// moved asks nothing), keeps its saved expiries and paints under its
/// saved view.
#[gpui::test]
fn a_restored_tile_requeries_once_and_paints_its_saved_expiries(cx: &mut gpui::TestAppContext) {
    let mut t = launched_on("SPX.Z");
    t.insert(
        "expiries".into(),
        toml::Value::Array(vec![toml::Value::String("2026-12-18".into())]),
    );
    t.insert(
        "view".into(),
        toml::Value::Array(vec![toml::Value::Float(0.95), toml::Value::Float(1.05)]),
    );
    let (h, mut vcx) = open_on(cx, t);
    h.show(&mut vcx);
    vcx.update(|_, cx| h.content.set_visible(false, cx));
    vcx.update(|_, cx| h.content.set_visible(true, cx));
    vcx.run_until_parked();
    let (doc, chains) = published();
    let reqs = h.requests();
    assert_eq!(docs(&reqs).len(), 1, "{reqs:?}");
    let tag = docs(&reqs)[0].tag;
    h.answer_doc(&mut vcx, tag, cvi_snapshot(&doc));
    let _ = h.requests();
    h.answer_doc(&mut vcx, tag, chain_snapshot(&chains));
    let reqs = h.requests();
    let params = vols(&reqs)[0].clone();
    h.answer_vol(&mut vcx, &params);
    assert_eq!(h.labels(&vcx), vec!["cvi 2026-12-18".to_string(),]);
    let view = h.tile.read_with(&vcx, |t, _| t.view()).expect("a view");
    assert_eq!((view.lo, view.hi), (0.95, 1.05), "the saved view stands");
}

/// A restored pair naming the draft, with no draft on any board, asks no
/// difference and says why in the footer rather than painting nothing.
#[gpui::test]
fn a_restored_pair_naming_an_unloaded_kind_is_a_notice(cx: &mut gpui::TestAppContext) {
    let mut t = launched_on("SPX.Z");
    t.insert(
        "diff".into(),
        toml::Value::Array(vec![
            toml::Value::String("cvi draft".into()),
            toml::Value::String("cvi".into()),
        ]),
    );
    let (h, mut vcx) = open_on(cx, t);
    h.show(&mut vcx);
    let (doc, chains) = published();
    let reqs = h.answer_documents(&mut vcx, &doc, &chains);
    let params = vols(&reqs)[0].clone();
    h.answer_vol(&mut vcx, &params);
    assert_eq!(
        h.notices(&vcx),
        vec!["diff cvi draft \u{2212} cvi: cvi draft is not loaded".to_string()]
    );
}

/// Hiding keeps the documents' query in flight; closing cancels by the
/// tile's key once.
#[gpui::test]
fn hide_keeps_the_query_and_close_cancels(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_on(cx, launched_on("SPX.Z"));
    h.show(&mut vcx);
    assert_eq!(docs(&h.requests()).len(), 1);
    let cancels = |reqs: &[Request]| {
        reqs.iter()
            .filter(|r| matches!(r, Request::Cancel { key } if *key == KEY))
            .count()
    };
    vcx.update(|_, cx| h.content.set_visible(false, cx));
    assert_eq!(cancels(&h.requests()), 0, "hiding keeps the query");
    vcx.update(|_, cx| h.content.closed(cx));
    assert_eq!(cancels(&h.requests()), 1, "closing cancels by key, once");
}

impl Harness {
    /// How many times frame observers have heard the frame since now.
    fn frame_heard(
        &self,
        vcx: &mut gpui::VisualTestContext,
    ) -> (Rc<std::cell::Cell<usize>>, gpui::Subscription) {
        let heard = Rc::new(std::cell::Cell::new(0));
        let count = heard.clone();
        let sub =
            vcx.update(|_, cx| cx.observe(&self.frame, move |_, _| count.set(count.get() + 1)));
        (heard, sub)
    }
}

/// A show from inside the shell's draw whose read is refused answers the
/// open flip through a deferred arrival: the release happens and frame
/// observers hear it, so every other tile promotes now rather than at the
/// barrier's deadline. Inline, the release's notify falls in the draw and
/// is dropped.
#[gpui::test]
fn a_refused_show_from_the_draw_releases_the_flip_to_frame_observers(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_on(cx, launched_on("SPX.Z"));
    h.tile.update(&mut vcx, |t, _| t.today_pin = Some(d(TODAY)));
    h.open_flip(&mut vcx);
    assert!(h.barrier_open(&vcx), "the flip awaits the tile");
    let (heard, _sub) = h.frame_heard(&mut vcx);
    h.data.fill_for_tests();
    h.in_draw(&mut vcx, Some(true), None);
    vcx.run_until_parked();
    assert!(
        h.notices(&vcx)
            .contains(&"document request refused: the data service is busy".to_string())
    );
    assert!(!h.barrier_open(&vcx), "the refusal arrived");
    assert!(heard.get() > 0, "frame observers heard the release");
}

/// A tile the shell closes from inside its draw, while the flip awaits its
/// query, answers the flip through a deferred arrival that frame observers
/// hear.
#[gpui::test]
fn a_close_from_the_draw_releases_the_flip_to_frame_observers(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_on(cx, launched_on("SPX.Z"));
    h.show(&mut vcx);
    assert_eq!(docs(&h.requests()).len(), 1, "a query in flight");
    h.open_flip(&mut vcx);
    assert!(h.barrier_open(&vcx));
    let (heard, _sub) = h.frame_heard(&mut vcx);
    h.close_in_draw(&mut vcx);
    vcx.run_until_parked();
    assert!(!h.barrier_open(&vcx), "the closing tile arrived");
    assert!(heard.get() > 0, "frame observers heard the release");
}

/// A viewer reads a group; it posts nothing into one.
#[gpui::test]
fn the_viewer_emits_nothing(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    assert!(!h.content.emits());
    assert_eq!(
        vcx.update(|_, cx| h.content.emission(cx)),
        Emission::default()
    );
}

mod keys;

mod paint;

mod picture;
