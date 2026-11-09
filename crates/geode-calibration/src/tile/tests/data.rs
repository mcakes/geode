//! The data flow: the source documents asked in sequence under one tag,
//! the run submitted when every source is in, outcomes merged by tag, a
//! followed group's scope flip, publications, visibility and refusals.
//! Every test drives the shell's doors: deliveries through
//! `TileContent::deliver`, frame changes through the frame's test-support
//! entry points, requests observed on the data handle's receiver.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{NaiveDate, TimeZone, Utc};
use geode_core::calibration::{
    BUILTIN_CALIBRATION, CalibrationOutcome, REQUEST_STAGE, RunAction, StagePayload, StageResult,
    StageStatus,
};
use geode_core::document::test_support::snapshot_of_documents;
use geode_core::document::{Column, DocumentRows, Value, join_key};
use geode_core::link::{Group, Membership, Posting, underlying_scope};
use geode_core::query::{DocumentParams, QueryKey, QueryOutcome};
use geode_core::snapshot::Snapshot;
use geode_data::Request;
use geode_shell::frame::{FrameRef, Publish};
use geode_shell::module::Delivery;
use geode_shell::tiling::{TileId, WorkspaceIx};

use super::harness::{Harness, builtin_config, config_from, demo_schema, open_with, restored};
use crate::core::model::Tone;

pub(super) const KEY: QueryKey = QueryKey(Harness::TILE);
const EMITTER: TileId = TileId(9);

/// One document read as asked: its dataset, key and tag.
type Asked = (String, Vec<String>, u64);

fn d(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

/// One expiry of an option chain: six strikes around spot.
fn chain_doc(u: &str, expiry: &str) -> DocumentRows {
    let strikes: Vec<f64> = (0..6).map(|i| 4700.0 + 100.0 * i as f64).collect();
    let n = strikes.len();
    DocumentRows {
        key: vec![u.into(), expiry.into()],
        attributes: vec![
            ("forward".into(), Value::F64(5050.0)),
            ("spot_ref".into(), Value::F64(5000.0)),
            (
                "quote_time".into(),
                Value::Utf8("2026-11-09T15:30:00Z".into()),
            ),
        ],
        axes: vec![("strike".into(), Column::F64(strikes.clone()))],
        values: vec![
            ("bid_vol".into(), Column::F64(vec![0.18; n])),
            ("ask_vol".into(), Column::F64(vec![0.20; n])),
            ("mid_vol".into(), Column::F64(vec![0.19; n])),
            (
                "bid".into(),
                Column::F64(strikes.iter().map(|k| (5050.0 - k).max(1.0)).collect()),
            ),
            (
                "ask".into(),
                Column::F64(
                    strikes
                        .iter()
                        .map(|k| (5050.0 - k).max(1.0) + 2.0)
                        .collect(),
                ),
            ),
        ],
    }
}

fn dividends_doc(u: &str) -> DocumentRows {
    DocumentRows {
        key: vec![u.into()],
        attributes: vec![
            ("currency".into(), Value::Utf8("USD".into())),
            ("schedule_date".into(), Value::Date(d("2026-11-09"))),
        ],
        axes: vec![(
            "dividend_id".into(),
            Column::Utf8(vec!["d1".into(), "d2".into()]),
        )],
        values: vec![
            (
                "ex_date".into(),
                Column::Date(vec![d("2026-12-15"), d("2027-03-15")]),
            ),
            (
                "announced_date".into(),
                Column::Date(vec![d("2026-11-01"), d("2026-11-01")]),
            ),
            (
                "pay_date".into(),
                Column::Date(vec![d("2026-12-20"), d("2027-03-20")]),
            ),
            ("amount".into(), Column::F64(vec![12.5, 13.0])),
            (
                "status".into(),
                Column::Utf8(vec!["confirmed".into(), "forecast".into()]),
            ),
        ],
    }
}

pub(super) fn rates_doc(u: &str, series: &str) -> DocumentRows {
    DocumentRows {
        key: vec![u.into(), series.into()],
        attributes: vec![(
            "published_time".into(),
            Value::Utf8("2026-11-09T07:00:00Z".into()),
        )],
        axes: vec![("tenor".into(), Column::F64(vec![0.25, 1.0, 5.0]))],
        values: vec![("value".into(), Column::F64(vec![0.04, 0.041, 0.045]))],
    }
}

/// The snapshot the data tier answers a prefix read of `dataset` under
/// `u` with: every document of the underlying, contiguous.
fn snapshot_for(dataset: &str, u: &str) -> Snapshot {
    let schema = demo_schema();
    let ds = schema.dataset(dataset).expect("a demo dataset");
    let docs: Vec<DocumentRows> = match dataset {
        "option_chain" => vec![chain_doc(u, "2026-12-18"), chain_doc(u, "2027-01-15")],
        "dividend_schedule" => vec![dividends_doc(u)],
        "rate_curve" => vec![rates_doc(u, "funding")],
        other => panic!("no fixture for {other}"),
    };
    let refs: Vec<&DocumentRows> = docs.iter().collect();
    snapshot_of_documents(ds, &refs, |_| {})
}

pub(super) fn docs(reqs: &[Request]) -> Vec<&DocumentParams> {
    reqs.iter()
        .filter_map(|r| match r {
            Request::Document(p) => Some(p),
            _ => None,
        })
        .collect()
}

pub(super) fn calibrates(reqs: &[Request]) -> Vec<&geode_core::calibration::CalibrationRequest> {
    reqs.iter()
        .filter_map(|r| match r {
            Request::Calibrate(p) => Some(p),
            _ => None,
        })
        .collect()
}

pub(super) fn result(name: &str, summary: &str) -> StageResult {
    StageResult {
        name: name.into(),
        status: StageStatus::Ok,
        ran_at: Some(Utc.with_ymd_and_hms(2026, 11, 9, 9, 30, 15).unwrap()),
        elapsed: Duration::from_millis(3),
        summary: summary.into(),
        payload: StagePayload::None,
    }
}

pub(super) fn run_outcome(key: QueryKey, tag: u64, stages: Vec<StageResult>) -> CalibrationOutcome {
    CalibrationOutcome {
        key,
        tag,
        submitted: Instant::now(),
        action: RunAction::Run { from: None },
        stages,
        publish_rows: None,
    }
}

/// The builtin document with the pipeline switched to `run = "manual"`.
fn manual_config() -> crate::content::CalibrationConfig {
    let text = BUILTIN_CALIBRATION.replace("run   = \"on_input\"", "run   = \"manual\"");
    assert_ne!(
        text, BUILTIN_CALIBRATION,
        "the fixture flipped the run mode"
    );
    let config = config_from(&text);
    assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
    config
}

impl Harness {
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

    fn fail_doc(&self, vcx: &mut gpui::VisualTestContext, tag: u64, error: &str) {
        self.deliver(
            vcx,
            Delivery::Query(QueryOutcome {
                key: KEY,
                tag,
                snapshot: Err(error.to_string()),
                submitted: Instant::now(),
            }),
        );
    }

    /// Answer the one document read out, if any, with `u`'s fixture for
    /// its dataset; `None` when nothing is out. Returns the request's
    /// dataset, key and tag, and every other request found beside it.
    fn answer_next(
        &self,
        vcx: &mut gpui::VisualTestContext,
        u: &str,
    ) -> (Option<Asked>, Vec<Request>) {
        let reqs = self.requests();
        self.answer_batch(vcx, reqs, u)
    }

    /// `answer_next` over a batch of requests already taken from the
    /// channel.
    fn answer_batch(
        &self,
        vcx: &mut gpui::VisualTestContext,
        reqs: Vec<Request>,
        u: &str,
    ) -> (Option<Asked>, Vec<Request>) {
        let (reads, others): (Vec<Request>, Vec<Request>) = reqs
            .into_iter()
            .partition(|r| matches!(r, Request::Document(_)));
        assert!(reads.len() <= 1, "one read at a time: {reads:?}");
        let Some(Request::Document(p)) = reads.first() else {
            return (None, others);
        };
        assert!(
            calibrates(&others).is_empty(),
            "no run while a read is out: {others:?}"
        );
        assert_eq!(p.key, KEY);
        let asked = (p.dataset.clone(), p.document_key.clone(), p.tag);
        self.answer_doc(vcx, p.tag, snapshot_for(&p.dataset, u));
        (Some(asked), others)
    }

    /// Answer every read in the sequence for `u`; returns what was asked, in
    /// order, and every other request the tile made along the way.
    pub(super) fn answer_all(
        &self,
        vcx: &mut gpui::VisualTestContext,
        u: &str,
    ) -> (Vec<Asked>, Vec<Request>) {
        let reqs = self.requests();
        self.answer_all_from(vcx, reqs, u)
    }

    /// `answer_all`, beginning with a batch already taken from the channel.
    pub(super) fn answer_all_from(
        &self,
        vcx: &mut gpui::VisualTestContext,
        reqs: Vec<Request>,
        u: &str,
    ) -> (Vec<Asked>, Vec<Request>) {
        let mut asked = Vec::new();
        let mut others = Vec::new();
        let mut batch = reqs;
        loop {
            let (next, more) = self.answer_batch(vcx, batch, u);
            others.extend(more);
            match next {
                Some(a) => asked.push(a),
                None => return (asked, others),
            }
            batch = self.requests();
        }
    }

    /// A loaded tile: every source answered and the run's request taken.
    pub(super) fn load(&self, vcx: &mut gpui::VisualTestContext, u: &str) -> u64 {
        let (asked, after) = self.answer_all(vcx, u);
        assert_eq!(asked.len(), 3, "{asked:?}");
        let runs = calibrates(&after);
        assert_eq!(runs.len(), 1, "{after:?}");
        runs[0].tag
    }

    pub(super) fn card_status(&self, vcx: &gpui::VisualTestContext, stage: &str) -> String {
        self.tile.read_with(vcx, |t, _| {
            let model = t.model().expect("a pipeline is shown");
            model
                .cards
                .iter()
                .find(|c| c.stage == stage)
                .unwrap_or_else(|| panic!("{stage} has no card"))
                .status
                .to_string()
        })
    }

    pub(super) fn card_tone(&self, vcx: &gpui::VisualTestContext, stage: &str) -> Tone {
        self.tile.read_with(vcx, |t, _| {
            t.model()
                .expect("a pipeline is shown")
                .cards
                .iter()
                .find(|c| c.stage == stage)
                .unwrap_or_else(|| panic!("{stage} has no card"))
                .tone
        })
    }

    pub(super) fn footer(&self, vcx: &gpui::VisualTestContext) -> String {
        self.tile.read_with(vcx, |t, _| {
            t.model().map(|m| m.footer.to_string()).unwrap_or_default()
        })
    }

    fn card_stages(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.tile.read_with(vcx, |t, _| {
            t.model()
                .map(|m| m.cards.iter().map(|c| c.stage.clone()).collect())
                .unwrap_or_default()
        })
    }

    pub(super) fn result_names(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.tile.read_with(vcx, |t, _| t.result_names())
    }

    pub(super) fn set_visible(&self, vcx: &mut gpui::VisualTestContext, visible: bool) {
        vcx.update(|_, cx| self.content.set_visible(visible, cx));
        vcx.run_until_parked();
    }

    fn link(&self, vcx: &mut gpui::VisualTestContext, tile: TileId, membership: Membership) {
        self.frame.update(vcx, |f, cx| {
            f.link_for_test(tile, membership);
            cx.notify();
        });
        vcx.run_until_parked();
    }

    /// The emitter emits into A, then the tile follows A.
    pub(super) fn follow_a(&self, vcx: &mut gpui::VisualTestContext) {
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
            TileId(Harness::TILE),
            Membership {
                follow: Some(Group::A),
                emit: None,
            },
        );
    }

    /// The emitter names `u` in group A's scope; the shell enrolls the
    /// follower under its own reading, as `extend_flip` does.
    pub(super) fn flip_to(&self, vcx: &mut gpui::VisualTestContext, u: &str) {
        let bound = FrameRef::for_tile(
            self.frame.clone(),
            WorkspaceIx::FIRST,
            TileId(Harness::TILE),
        );
        bound.update(vcx, |f, cx| {
            f.post_for_test(
                EMITTER,
                Posting {
                    scope: Some(underlying_scope(u)),
                    board: Vec::new(),
                },
            );
            f.open_flip([KEY], Instant::now());
            cx.notify();
        });
        vcx.run_until_parked();
    }

    fn barrier_open(&self, vcx: &gpui::VisualTestContext) -> bool {
        self.frame.read_with(vcx, |f, _| f.barrier_open())
    }

    /// A publish of `batch` in `dataset` reaches the frame, as the data
    /// bridge reports one.
    fn publish(&self, vcx: &mut gpui::VisualTestContext, dataset: &str, batch: &[&str]) {
        let batch = join_key(&batch.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        self.frame.update(vcx, |f, cx| {
            f.note_published(Publish {
                dataset: dataset.into(),
                batch,
                books: 0,
                at: Utc::now(),
            });
            cx.notify();
        });
        vcx.run_until_parked();
    }
}

fn open_spx(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    open_with(
        cx,
        Some(builtin_config()),
        Some(restored(Some("SPX"), None, None)),
    )
}

/// The query pool keeps one request per key, so the three source reads go
/// out in sequence under one tag: each dataset is asked for the
/// underlying's documents by key prefix, in the pipeline's order, and the
/// next read follows the last answer.
#[gpui::test]
fn opening_queries_every_source_dataset_for_the_underlying(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx);
    // Every source waits from the first ask.
    for stage in ["chain", "dividends", "rates"] {
        assert_eq!(h.card_status(&vcx, stage), "waiting");
        assert_eq!(h.card_tone(&vcx, stage), Tone::Waiting);
    }
    assert_eq!(h.footer(&vcx), "waiting for chain, dividends, rates");
    let (asked, _) = h.answer_all(&mut vcx, "SPX");
    let datasets: Vec<&str> = asked.iter().map(|(d, _, _)| d.as_str()).collect();
    assert_eq!(
        datasets,
        ["option_chain", "dividend_schedule", "rate_curve"]
    );
    for (_, key, tag) in &asked {
        assert_eq!(key, &vec!["SPX".to_string()]);
        assert_eq!(*tag, asked[0].2, "one tag for the sequence");
    }
}

/// Two of three sources in: the pipeline cannot run, the cards say which
/// source is missing, and the footer names it before anything else.
#[gpui::test]
fn no_run_is_submitted_until_every_source_has_arrived(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx);
    h.answer_next(&mut vcx, "SPX")
        .0
        .expect("the chain is asked");
    h.answer_next(&mut vcx, "SPX")
        .0
        .expect("the dividends are asked");
    let reqs = h.requests();
    assert!(calibrates(&reqs).is_empty(), "{reqs:?}");
    assert_eq!(docs(&reqs).len(), 1, "the rates read is out: {reqs:?}");
    assert_eq!(docs(&reqs)[0].dataset, "rate_curve");
    assert_eq!(h.card_status(&vcx, "chain"), "not run");
    assert_eq!(h.card_tone(&vcx, "chain"), Tone::Plain);
    assert_eq!(h.card_status(&vcx, "dividends"), "not run");
    assert_eq!(h.card_status(&vcx, "rates"), "waiting");
    assert_eq!(h.card_tone(&vcx, "rates"), Tone::Waiting);
    assert_eq!(h.footer(&vcx), "waiting for rates");
    assert!(h.result_names(&vcx).is_empty());
}

/// Every source in: one run goes out with every source's documents in
/// pipeline order, the chain's two expiries as two rows, keyed by the tile
/// and tagged by the tile's run counter; its answer fills the cards.
#[gpui::test]
fn a_complete_source_set_submits_one_run_with_every_input(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx);
    let (_, after) = h.answer_all(&mut vcx, "SPX");
    let runs = calibrates(&after);
    assert_eq!(runs.len(), 1, "{after:?}");
    let run = runs[0];
    assert_eq!(run.key, KEY);
    assert_eq!(run.underlying, "SPX");
    assert_eq!(run.pipeline.name, "implied_forwards");
    assert_eq!(run.action, RunAction::Run { from: None });
    let inputs: Vec<(&str, usize)> = run
        .inputs
        .iter()
        .map(|i| (i.stage.as_str(), i.rows.len()))
        .collect();
    assert_eq!(inputs, [("chain", 2), ("dividends", 1), ("rates", 1)]);
    assert_eq!(
        run.inputs[0].rows[1].key,
        vec!["SPX".to_string(), "2027-01-15".to_string()]
    );
    assert_eq!(
        run.inputs[2].rows[0].key,
        vec!["SPX".to_string(), "funding".to_string()]
    );
    assert_eq!(h.footer(&vcx), "", "nothing waits");

    h.deliver(
        &mut vcx,
        Delivery::Calibration(run_outcome(
            KEY,
            run.tag,
            vec![result("chain", "2 docs"), result("parity", "6 strikes")],
        )),
    );
    assert_eq!(h.card_status(&vcx, "chain"), "09:30:15 · 2 docs");
    assert_eq!(h.card_status(&vcx, "parity"), "09:30:15 · 6 strikes");
    assert_eq!(h.card_status(&vcx, "rates"), "not run");
    assert_eq!(h.result_names(&vcx), vec!["chain", "parity"]);
}

/// A publish of one source's dataset under the underlying asks that source
/// again and no other; the others' rows are kept and the run goes out
/// again with all three. A publish under another underlying asks nothing.
#[gpui::test]
fn a_publication_refreshes_only_that_source_and_runs_again(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx);
    let first = h.load(&mut vcx, "SPX");
    h.publish(&mut vcx, "dividend_schedule", &["SPX"]);
    let reqs = h.requests();
    let asked = docs(&reqs);
    assert_eq!(asked.len(), 1, "{reqs:?}");
    assert_eq!(asked[0].dataset, "dividend_schedule");
    assert!(calibrates(&reqs).is_empty());
    assert_eq!(h.card_status(&vcx, "dividends"), "waiting");
    assert_eq!(h.card_status(&vcx, "chain"), "not run", "the chain is kept");
    assert_eq!(h.footer(&vcx), "waiting for dividends");
    let (asked, after) = h.answer_all_from(&mut vcx, reqs, "SPX");
    assert_eq!(asked.len(), 1);
    let runs = calibrates(&after);
    assert_eq!(runs.len(), 1, "{after:?}");
    assert!(runs[0].tag > first, "a newer run");
    let inputs: Vec<(&str, usize)> = runs[0]
        .inputs
        .iter()
        .map(|i| (i.stage.as_str(), i.rows.len()))
        .collect();
    assert_eq!(inputs, [("chain", 2), ("dividends", 1), ("rates", 1)]);

    // An expiry of another underlying's chain is not this tile's.
    h.publish(&mut vcx, "option_chain", &["NDX", "2026-12-18"]);
    assert!(h.requests().is_empty());
    // An expiry of this underlying's chain is.
    h.publish(&mut vcx, "option_chain", &["SPX", "2026-12-18"]);
    let reqs = h.requests();
    assert_eq!(docs(&reqs).len(), 1, "{reqs:?}");
    assert_eq!(docs(&reqs)[0].dataset, "option_chain");
}

/// An outcome under an older tag, or addressed to another tile, changes
/// nothing; the current tag's lands.
#[gpui::test]
fn a_stale_or_foreign_calibration_outcome_is_ignored(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx);
    let tag = h.load(&mut vcx, "SPX");
    assert!(tag >= 1);
    h.deliver(
        &mut vcx,
        Delivery::Calibration(run_outcome(KEY, tag - 1, vec![result("parity", "stale")])),
    );
    assert!(h.result_names(&vcx).is_empty(), "an older tag is dropped");
    assert_eq!(h.card_status(&vcx, "parity"), "not run");
    h.deliver(
        &mut vcx,
        Delivery::Calibration(run_outcome(
            QueryKey(99),
            tag,
            vec![result("parity", "foreign")],
        )),
    );
    assert!(h.result_names(&vcx).is_empty(), "another key is dropped");
    h.deliver(
        &mut vcx,
        Delivery::Calibration(run_outcome(KEY, tag, vec![result("parity", "6 strikes")])),
    );
    assert_eq!(h.card_status(&vcx, "parity"), "09:30:15 · 6 strikes");
}

/// The synthetic `request` stage is a refused or failed request: its
/// reason is a header notice, and no card is made for it.
#[gpui::test]
fn a_request_stage_outcome_is_a_notice_not_a_card(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx);
    let tag = h.load(&mut vcx, "SPX");
    let reason = "the calibration queue is full; resubmit";
    h.deliver(
        &mut vcx,
        Delivery::Calibration(run_outcome(
            KEY,
            tag,
            vec![StageResult {
                name: REQUEST_STAGE.into(),
                status: StageStatus::Failed(reason.into()),
                ran_at: None,
                elapsed: Duration::ZERO,
                summary: reason.into(),
                payload: StagePayload::None,
            }],
        )),
    );
    assert_eq!(h.notices(&vcx), vec![reason.to_string()]);
    assert!(h.result_names(&vcx).is_empty());
    assert!(
        !h.card_stages(&vcx).iter().any(|s| s == REQUEST_STAGE),
        "{:?}",
        h.card_stages(&vcx)
    );
    // The next good answer clears the notice.
    h.deliver(
        &mut vcx,
        Delivery::Calibration(run_outcome(KEY, tag, vec![result("parity", "6 strikes")])),
    );
    assert!(h.notices(&vcx).is_empty());
}

/// A followed group's scope moving to another underlying clears the last
/// underlying's results and documents at once, so no card shows SPX's fit
/// under NDX's name; the sources are asked again for NDX, the flip waits
/// on this tile until they are in, and SPX's late run answer is dropped.
#[gpui::test]
fn a_scope_flip_clears_results_and_requeries(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, Some(builtin_config()), None);
    h.follow_a(&mut vcx);
    h.flip_to(&mut vcx, "SPX");
    assert_eq!(h.title(&vcx), "Calibration \u{b7} SPX");
    let spx_tag = h.load(&mut vcx, "SPX");
    h.deliver(
        &mut vcx,
        Delivery::Calibration(run_outcome(
            KEY,
            spx_tag,
            vec![result("chain", "2 docs"), result("parity", "6 strikes")],
        )),
    );
    assert_eq!(h.card_status(&vcx, "parity"), "09:30:15 · 6 strikes");
    assert!(!h.barrier_open(&vcx));

    h.flip_to(&mut vcx, "NDX");
    assert_eq!(h.title(&vcx), "Calibration \u{b7} NDX");
    assert!(h.result_names(&vcx).is_empty(), "SPX's results are gone");
    assert_eq!(h.card_status(&vcx, "parity"), "not run");
    assert_eq!(h.card_status(&vcx, "chain"), "waiting");
    assert_eq!(h.footer(&vcx), "waiting for chain, dividends, rates");
    assert!(h.barrier_open(&vcx), "the flip waits on this tile");
    let reqs = h.requests();
    let asked = docs(&reqs);
    assert_eq!(asked.len(), 1, "{reqs:?}");
    assert_eq!(asked[0].document_key, vec!["NDX".to_string()]);
    assert!(calibrates(&reqs).is_empty());

    // SPX's run answering now is an answer to a question nobody asks.
    h.deliver(
        &mut vcx,
        Delivery::Calibration(run_outcome(
            KEY,
            spx_tag,
            vec![result("parity", "late SPX")],
        )),
    );
    assert!(h.result_names(&vcx).is_empty());

    let (asked, after) = h.answer_all_from(&mut vcx, reqs, "NDX");
    assert_eq!(asked.len(), 3);
    assert!(
        !h.barrier_open(&vcx),
        "the last source's arrival released it"
    );
    let runs = calibrates(&after);
    assert_eq!(runs.len(), 1, "{after:?}");
    assert_eq!(runs[0].underlying, "NDX");
    assert!(runs[0].tag > spx_tag);
    assert_eq!(runs[0].inputs[0].rows[0].key[0], "NDX");
}

/// A run the data handle refuses (its queue full) is a status notice; the
/// documents stay, so the next change retries.
#[gpui::test]
fn a_refused_submission_shows_the_queue_full_notice(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx);
    h.answer_next(&mut vcx, "SPX").0.expect("the chain");
    h.answer_next(&mut vcx, "SPX").0.expect("the dividends");
    let reqs = h.requests();
    let rates = docs(&reqs)[0];
    let tag = rates.tag;
    h.data.fill_for_tests();
    h.answer_doc(&mut vcx, tag, snapshot_for("rate_curve", "SPX"));
    assert_eq!(h.notices(&vcx), vec!["calibration queue full".to_string()]);
    assert_eq!(h.footer(&vcx), "", "every source is in");
    assert_eq!(h.card_status(&vcx, "rates"), "not run");
    // Draining the channel: nothing of the tile's went out after the fill.
    let after = h.requests();
    assert!(calibrates(&after).is_empty(), "{after:?}");
}

/// Hidden, the tile keeps reading (its answers land) but submits no run;
/// shown again with nothing changed it submits the run that was due,
/// once, without asking the documents again.
#[gpui::test]
fn a_hidden_tile_submits_nothing_and_runs_once_on_show(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx);
    h.set_visible(&mut vcx, false);
    let (asked, after) = h.answer_all(&mut vcx, "SPX");
    assert_eq!(asked.len(), 3, "the reads continue while hidden");
    assert!(
        calibrates(&after).is_empty(),
        "no run while hidden: {after:?}"
    );
    assert_eq!(h.footer(&vcx), "");
    h.set_visible(&mut vcx, true);
    let reqs = h.requests();
    assert_eq!(calibrates(&reqs).len(), 1, "{reqs:?}");
    assert!(docs(&reqs).is_empty(), "nothing is asked again: {reqs:?}");
    h.set_visible(&mut vcx, false);
    h.set_visible(&mut vcx, true);
    assert!(h.requests().is_empty(), "shown again with nothing due");
}

/// `run = "manual"`: a complete source set waits for the verb.
#[gpui::test]
fn manual_run_mode_waits_for_the_verb(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        Some(manual_config()),
        Some(restored(Some("SPX"), None, None)),
    );
    let (asked, after) = h.answer_all(&mut vcx, "SPX");
    assert_eq!(asked.len(), 3);
    assert!(calibrates(&after).is_empty(), "{after:?}");
    assert_eq!(
        h.footer(&vcx),
        "",
        "nothing waits; the verb is the trader's"
    );
    assert_eq!(h.card_status(&vcx, "rates"), "not run");
}

/// A dataset that has never published answers zero rows, not an error.
/// That source is still owed: its card waits, the footer names it, a
/// status notice says which dataset has nothing for the underlying, and
/// no run goes out with its input empty. The dataset's next publication
/// asks it again.
#[gpui::test]
fn an_empty_document_answer_leaves_its_source_waiting(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx);
    h.answer_next(&mut vcx, "SPX").0.expect("the chain");
    let reqs = h.requests();
    let dividends = docs(&reqs)[0];
    assert_eq!(dividends.dataset, "dividend_schedule");
    let schema = demo_schema();
    let empty = snapshot_of_documents(schema.dataset("dividend_schedule").unwrap(), &[], |_| {});
    assert_eq!(empty.rows(), 0);
    h.answer_doc(&mut vcx, dividends.tag, empty);
    let (asked, after) = h.answer_all(&mut vcx, "SPX");
    assert_eq!(asked.len(), 1, "the rates read still goes out: {asked:?}");
    assert!(
        calibrates(&after).is_empty(),
        "no run with an empty input: {after:?}"
    );
    assert_eq!(h.card_status(&vcx, "dividends"), "waiting");
    assert_eq!(h.card_tone(&vcx, "dividends"), Tone::Waiting);
    assert_eq!(h.card_status(&vcx, "chain"), "not run");
    assert_eq!(h.card_status(&vcx, "rates"), "not run");
    assert_eq!(h.footer(&vcx), "waiting for dividends");
    assert_eq!(
        h.notices(&vcx),
        vec!["dividend_schedule has no document for SPX".to_string()]
    );
    // The dataset publishes: the source is asked again and, answered, the
    // run goes out.
    h.publish(&mut vcx, "dividend_schedule", &["SPX"]);
    let (asked, after) = h.answer_all(&mut vcx, "SPX");
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].0, "dividend_schedule");
    assert_eq!(calibrates(&after).len(), 1, "{after:?}");
    assert!(h.notices(&vcx).is_empty(), "the notice is gone");
    assert_eq!(h.footer(&vcx), "");
}

/// A read that fails leaves its source waiting with the failure as a
/// notice; the other reads still go out, and no run is submitted with a
/// source missing. The flip is still answered: one broken read must not
/// hold every other tile to the deadline.
#[gpui::test]
fn a_failed_source_read_is_a_notice_and_no_run_goes_out(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx);
    h.answer_next(&mut vcx, "SPX").0.expect("the chain");
    let reqs = h.requests();
    let dividends = docs(&reqs)[0];
    assert_eq!(dividends.dataset, "dividend_schedule");
    h.fail_doc(&mut vcx, dividends.tag, "store closed");
    let (asked, after) = h.answer_all(&mut vcx, "SPX");
    assert_eq!(asked.len(), 1, "the rates read still goes out: {asked:?}");
    assert!(calibrates(&after).is_empty(), "{after:?}");
    assert_eq!(h.card_status(&vcx, "dividends"), "waiting");
    assert_eq!(h.card_status(&vcx, "rates"), "not run");
    assert_eq!(h.footer(&vcx), "waiting for dividends");
    assert_eq!(h.notices(&vcx), vec!["dividends: store closed".to_string()]);
}
