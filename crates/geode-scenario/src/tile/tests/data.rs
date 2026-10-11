//! The data flow: the axis domains, the main query at full depth and the
//! totals query run in sequence under one tag; the trio is one barrier
//! arrival; following, hiding, closing and the picture's notices. Every
//! test drives the shell's doors.

use super::*;
use crate::core::pivot::fixtures::{ladder_main, vega_main, vega_totals};
use geode_core::link::{Group, Membership, Posting, underlying_scope};
use geode_core::query::{AsOf, DistinctOutcome, QueryKey, QueryOutcome};
use geode_core::snapshot::Snapshot;
use geode_data::QueryParams;
use geode_shell::frame::Publish;
use geode_shell::module::Delivery;
use std::time::Instant;

const EMITTER: TileId = TileId(9);
const KEY: QueryKey = QueryKey(TILE);

/// A tile whose frame handle is bound to it, as the shell binds every
/// occupant's, over a checked builtin shape. The helpers below are
/// `pub(super)` so `chrome.rs` drives the same doors.
pub(super) fn open_bound(
    cx: &mut gpui::TestAppContext,
    shape: &str,
    restored: Option<toml::Table>,
) -> (Harness, gpui::VisualTestContext) {
    open_framed(
        cx,
        crate::core::spec::builtin_shape_checked(shape),
        restored,
        |frame| FrameRef::for_tile(frame, WorkspaceIx::FIRST, TileId(TILE)),
    )
}

/// [`open_bound`] over a fixture shape (already checked).
pub(super) fn open_bound_with(
    cx: &mut gpui::TestAppContext,
    shape: geode_core::scenario::ShapeSpec,
    restored: Option<toml::Table>,
) -> (Harness, gpui::VisualTestContext) {
    open_framed(cx, Arc::new(shape), restored, |frame| {
        FrameRef::for_tile(frame, WorkspaceIx::FIRST, TileId(TILE))
    })
}

pub(super) fn queries(reqs: &[Request]) -> Vec<&QueryParams> {
    reqs.iter()
        .filter_map(|r| match r {
            Request::Query(q) => Some(q),
            _ => None,
        })
        .collect()
}

pub(super) fn distincts(reqs: &[Request]) -> Vec<&geode_core::query::DistinctParams> {
    reqs.iter()
        .filter_map(|r| match r {
            Request::Distinct(d) => Some(d),
            _ => None,
        })
        .collect()
}

pub(super) fn cancels(reqs: &[Request]) -> usize {
    reqs.iter()
        .filter(|r| matches!(r, Request::Cancel { key } if *key == KEY))
        .count()
}

pub(super) fn scope_of(u: &str) -> Posting {
    Posting {
        scope: Some(underlying_scope(u)),
        board: Vec::new(),
    }
}

impl Harness {
    // Every helper here is `pub(super)`: chrome.rs drives the same doors.
    pub(super) fn show(&self, vcx: &mut gpui::VisualTestContext) {
        vcx.update(|_, cx| self.content.set_visible(true, cx));
        vcx.run_until_parked();
    }

    pub(super) fn deliver(&self, vcx: &mut gpui::VisualTestContext, delivery: Delivery) {
        vcx.update(|window, cx| self.content.deliver(delivery, window, cx));
        vcx.run_until_parked();
    }

    pub(super) fn answer_distinct(
        &self,
        vcx: &mut gpui::VisualTestContext,
        tag: u64,
        column: &str,
        labels: &[&str],
    ) {
        self.deliver(
            vcx,
            Delivery::Distinct(DistinctOutcome {
                key: KEY,
                tag,
                column: column.into(),
                values: Ok(labels.iter().map(|l| (l.to_string(), 1)).collect()),
            }),
        );
    }

    pub(super) fn answer_query(
        &self,
        vcx: &mut gpui::VisualTestContext,
        tag: u64,
        snapshot: Snapshot,
    ) {
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

    pub(super) fn fail_query(&self, vcx: &mut gpui::VisualTestContext, tag: u64, text: &str) {
        self.deliver(
            vcx,
            Delivery::Query(QueryOutcome {
                key: KEY,
                tag,
                snapshot: Err(text.into()),
                submitted: Instant::now(),
            }),
        );
    }

    /// Answer whatever is out for a vega K/T tile: the two domain reads
    /// (when asked), then main, then totals. The live query is the last
    /// one submitted. Returns the tag.
    pub(super) fn answer_vega(&self, vcx: &mut gpui::VisualTestContext) -> u64 {
        let reqs = self.requests();
        let mut tag = None;
        if let Some(d) = distincts(&reqs).last() {
            assert_eq!(d.column, "tenor");
            tag = Some(d.tag);
            self.answer_distinct(vcx, d.tag, "tenor", &["1Y", "18M", "2Y", "5Y"]);
            let reqs = self.requests();
            let d = distincts(&reqs)[0];
            assert_eq!(d.column, "moneyness");
            self.answer_distinct(vcx, d.tag, "moneyness", &["90.0", "100.0", "110.0"]);
        }
        let reqs = if tag.is_some() { self.requests() } else { reqs };
        let q = *queries(&reqs).last().expect("the main query is out");
        let tag = tag.unwrap_or(q.tag);
        assert_eq!(q.tag, tag);
        self.answer_query(vcx, tag, vega_main().snapshot("vega_buckets", Some(1)));
        let reqs = self.requests();
        let t = queries(&reqs)[0];
        assert_eq!(t.grouping.as_deref(), Some(&["moneyness".to_string()][..]));
        self.answer_query(vcx, tag, vega_totals().snapshot("vega_buckets", Some(1)));
        tag
    }

    /// Show, answer everything: a 4 × 3 picture (domains ∪ labels).
    pub(super) fn show_picture(&self, vcx: &mut gpui::VisualTestContext) {
        self.show(vcx);
        self.answer_vega(vcx);
        assert_eq!(
            self.delegate(vcx, |d| (d.layout.data_rows, d.layout.data_cols)),
            (4, 3)
        );
    }

    pub(super) fn notices(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.tile.read_with(vcx, |t, _| {
            t.reported_notices()
                .iter()
                .map(|n| n.text().to_string())
                .collect()
        })
    }

    pub(super) fn link(
        &self,
        vcx: &mut gpui::VisualTestContext,
        tile: TileId,
        membership: Membership,
    ) {
        self.frame.update(vcx, |f, cx| {
            f.link_for_test(tile, membership);
            cx.notify();
        });
        vcx.run_until_parked();
    }

    pub(super) fn post(&self, vcx: &mut gpui::VisualTestContext, posting: Posting) {
        self.frame.update(vcx, |f, cx| {
            f.post_for_test(EMITTER, posting);
            cx.notify();
        });
        vcx.run_until_parked();
    }

    /// The emitter emits to group A and the tile follows it. Following a
    /// group asks again at once; the caller drains that request.
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
            TileId(TILE),
            Membership {
                follow: Some(Group::A),
                emit: None,
            },
        );
    }

    pub(super) fn open_flip(&self, vcx: &mut gpui::VisualTestContext) {
        self.frame.update(vcx, |f, cx| {
            f.shared_mut().open_flip([KEY], Instant::now());
            cx.notify();
        });
        vcx.run_until_parked();
    }

    pub(super) fn barrier_open(&self, vcx: &gpui::VisualTestContext) -> bool {
        self.frame.read_with(vcx, |f, _| f.barrier_open())
    }

    pub(super) fn publish(&self, vcx: &mut gpui::VisualTestContext, dataset: &str) {
        self.frame.update(vcx, |f, cx| {
            f.note_published(Publish {
                dataset: dataset.into(),
                batch: "b".into(),
                books: 1,
                at: chrono::Utc::now(),
            });
            cx.notify();
        });
        vcx.run_until_parked();
    }
}

/// Spec §5.5 and §3.4 (P3-h): the first show asks each axis's domain,
/// unscoped and restricted to the dataset, then the main query grouped by
/// the shape's axes at full depth, then the totals query grouped by the
/// columns axes, all under one tag and the tile's key.
#[gpui::test]
fn a_show_asks_domains_then_main_then_totals_under_one_tag(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, "vega_kt", None);
    h.show(&mut vcx);
    let reqs = h.requests();
    let d = distincts(&reqs);
    assert_eq!(d.len(), 1, "one distinct at a time: {reqs:?}");
    assert_eq!(d[0].key, KEY);
    assert_eq!(d[0].column, "tenor");
    assert_eq!(d[0].dataset.as_deref(), Some("vega_buckets"));
    assert!(d[0].scope.is_empty(), "unscoped");
    let tag = d[0].tag;
    h.answer_distinct(&mut vcx, tag, "tenor", &["1Y"]);
    let reqs = h.requests();
    assert_eq!(distincts(&reqs)[0].column, "moneyness");
    h.answer_distinct(&mut vcx, tag, "moneyness", &["90.0"]);
    let reqs = h.requests();
    let q = queries(&reqs);
    assert_eq!(q.len(), 1);
    assert_eq!(q[0].view, "scenario:vega_kt");
    assert_eq!(
        q[0].grouping.as_deref(),
        Some(&["tenor".to_string(), "moneyness".to_string()][..])
    );
    assert_eq!(q[0].max_depth, 2, "full depth: the Σ rows ride along");
    assert_eq!(q[0].tag, tag);
    h.answer_query(&mut vcx, tag, vega_main().snapshot("vega_buckets", Some(1)));
    let reqs = h.requests();
    let t = queries(&reqs);
    assert_eq!(t.len(), 1);
    assert_eq!(
        t[0].grouping.as_deref(),
        Some(&["moneyness".to_string()][..])
    );
    assert_eq!(t[0].max_depth, 1);
    assert_eq!(t[0].tag, tag);
    h.answer_query(
        &mut vcx,
        tag,
        vega_totals().snapshot("vega_buckets", Some(1)),
    );
    assert_eq!(
        h.delegate(&vcx, |d| (d.layout.data_rows, d.layout.data_cols)),
        (3, 2)
    );
    assert!(h.delegate(&vcx, |d| d.layout.sigma_row && d.layout.sigma_col));
}

/// A ladder has no totals query: main lands and the picture is up.
#[gpui::test]
fn a_ladder_stops_at_the_main_query(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound_with(cx, crate::core::pivot::fixtures::spot_ladder(), None);
    h.show(&mut vcx);
    let reqs = h.requests();
    let tag = distincts(&reqs)[0].tag;
    h.answer_distinct(&mut vcx, tag, "spot_shift", &["-5.0", "0.0", "5.0"]);
    let reqs = h.requests();
    assert_eq!(queries(&reqs)[0].max_depth, 1);
    h.answer_query(
        &mut vcx,
        tag,
        ladder_main().snapshot("spot_ladder", Some(1)),
    );
    assert!(h.requests().is_empty(), "no totals for a ladder");
    assert_eq!(h.delegate(&vcx, |d| d.layout.data_cols), 3);
    assert!(
        h.delegate(&vcx, |d| d.pivot.as_ref().unwrap().base_row == Some(1)),
        "diff on, base 0.0"
    );
}

/// The chain is one barrier arrival: the flip stays open until totals land.
#[gpui::test]
fn the_barrier_sees_one_arrival_for_the_chain(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, "vega_kt", None);
    h.show(&mut vcx);
    h.open_flip(&mut vcx);
    assert!(h.barrier_open(&vcx));
    let reqs = h.requests();
    let tag = distincts(&reqs)
        .first()
        .map(|d| d.tag)
        .unwrap_or_else(|| queries(&reqs)[0].tag);
    h.answer_distinct(&mut vcx, tag, "tenor", &["1Y"]);
    h.answer_distinct(&mut vcx, tag, "moneyness", &["90.0"]);
    assert!(h.barrier_open(&vcx), "domains alone are not the answer");
    h.answer_query(&mut vcx, tag, vega_main().snapshot("vega_buckets", Some(1)));
    assert!(h.barrier_open(&vcx), "main alone is not the answer");
    let _ = h.requests();
    h.answer_query(
        &mut vcx,
        tag,
        vega_totals().snapshot("vega_buckets", Some(1)),
    );
    assert!(!h.barrier_open(&vcx), "the trio arrived once");
}

/// A scope-only change (the group the tile follows moves) reuses the
/// domains: the next chain starts at the main query.
#[gpui::test]
fn a_scope_only_change_reuses_the_domains(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, "vega_kt", None);
    h.show_picture(&mut vcx);
    h.follow_a(&mut vcx);
    let _ = h.requests();
    h.post(&mut vcx, scope_of("SPX"));
    let reqs = h.requests();
    assert!(distincts(&reqs).is_empty(), "no domain reads: {reqs:?}");
    let q = queries(&reqs);
    assert_eq!(q.len(), 1);
    assert_eq!(q[0].scope, underlying_scope("SPX"));
}

/// A publication on the dataset moves the data counter: the domains are
/// read again before the main query.
#[gpui::test]
fn a_data_change_rereads_the_domains(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, "vega_kt", None);
    h.show_picture(&mut vcx);
    h.publish(&mut vcx, "vega_buckets");
    let reqs = h.requests();
    assert_eq!(distincts(&reqs).len(), 1, "{reqs:?}");
    assert_eq!(distincts(&reqs)[0].column, "tenor");
}

/// A tile pinned with `:asof` reads its domains and rows at the pinned
/// instant and ignores the frame's as-of counter.
#[gpui::test]
fn a_pinned_as_of_ignores_the_frames_counter(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, "vega_kt", None);
    h.show_picture(&mut vcx);
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-09T15:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    vcx.update(|w, cx| h.content.command("asof 2026-10-09T15:00:00Z", w, cx))
        .unwrap();
    vcx.run_until_parked();
    let reqs = h.requests();
    let d = distincts(&reqs);
    assert_eq!(d.len(), 1, "the as-of moved: domains again: {reqs:?}");
    assert_eq!(d[0].as_of, AsOf::At(at));
    let tag = d[0].tag;
    h.answer_distinct(&mut vcx, tag, "tenor", &["1Y"]);
    h.answer_distinct(&mut vcx, tag, "moneyness", &["90.0"]);
    let reqs = h.requests();
    assert_eq!(queries(&reqs)[0].as_of, AsOf::At(at));
    h.answer_query(&mut vcx, tag, vega_main().snapshot("vega_buckets", Some(1)));
    let _ = h.requests();
    h.answer_query(
        &mut vcx,
        tag,
        vega_totals().snapshot("vega_buckets", Some(1)),
    );
    // The frame's as-of moves: a pinned tile does not ask.
    h.frame.update(&mut vcx, |f, cx| {
        f.shared_mut().set_as_of(AsOf::At(chrono::Utc::now()));
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(
        h.requests().is_empty(),
        "pinned: the frame's as-of is not followed"
    );
}

/// Following a group whose scope moves flips the follower (the shell
/// enrolls it under its own reading, as `extend_flip` does).
#[gpui::test]
fn a_group_scope_change_flips_the_follower(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, "vega_kt", None);
    h.show_picture(&mut vcx);
    h.follow_a(&mut vcx);
    let _ = h.requests();
    h.post(&mut vcx, scope_of("SPX"));
    let tag = h.answer_vega(&mut vcx);
    // Through the tile's own handle: the barrier enrolls the follower
    // under its group's reading, as the shell's `extend_flip` does. The
    // shared lane's reading is not the one a follower answers with.
    let bound = FrameRef::for_tile(h.frame.clone(), WorkspaceIx::FIRST, TileId(TILE));
    bound.update(&mut vcx, |f, cx| {
        f.post_for_test(EMITTER, scope_of("NDX"));
        f.open_flip([KEY], Instant::now());
        cx.notify();
    });
    vcx.run_until_parked();
    let reqs = h.requests();
    let q = queries(&reqs);
    assert_eq!(q.len(), 1, "{reqs:?}");
    assert_eq!(q[0].scope, underlying_scope("NDX"));
    assert!(q[0].tag > tag);
    assert!(h.barrier_open(&vcx));
    h.answer_query(
        &mut vcx,
        q[0].tag,
        vega_main().snapshot("vega_buckets", Some(1)),
    );
    let _ = h.requests();
    h.answer_query(
        &mut vcx,
        q[0].tag,
        vega_totals().snapshot("vega_buckets", Some(1)),
    );
    assert!(!h.barrier_open(&vcx), "the follower's arrival released it");
    assert!(
        h.title(&mut vcx).ends_with("· NDX"),
        "{}",
        h.title(&mut vcx)
    );
}

#[gpui::test]
fn hide_keeps_the_query_and_close_cancels(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, "vega_kt", None);
    h.show(&mut vcx);
    assert_eq!(distincts(&h.requests()).len(), 1);
    vcx.update(|_, cx| h.content.set_visible(false, cx));
    assert_eq!(cancels(&h.requests()), 0, "hiding keeps the query");
    vcx.update(|_, cx| h.content.closed(cx));
    assert_eq!(cancels(&h.requests()), 1, "closing cancels by key, once");
}

#[gpui::test]
fn a_close_from_the_draw_releases_the_flip_to_frame_observers(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, "vega_kt", None);
    h.show(&mut vcx);
    h.open_flip(&mut vcx);
    assert!(h.barrier_open(&vcx));
    let heard = std::rc::Rc::new(std::cell::Cell::new(0));
    let count = heard.clone();
    let _sub = vcx.update(|_, cx| cx.observe(&h.frame, move |_, _| count.set(count.get() + 1)));
    h.close_in_draw(&mut vcx);
    vcx.run_until_parked();
    assert!(!h.barrier_open(&vcx), "the closing tile arrived");
    assert!(heard.get() > 0, "frame observers heard the release");
}

#[gpui::test]
fn a_refused_submission_is_a_notice_and_the_barrier_is_answered(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, "vega_kt", None);
    h.open_flip(&mut vcx);
    h.data.fill_for_tests();
    h.in_draw(&mut vcx, Some(true), None);
    vcx.run_until_parked();
    assert!(
        h.notices(&vcx)
            .iter()
            .any(|n| n.contains("refused") && n.contains("busy")),
        "{:?}",
        h.notices(&vcx)
    );
    assert!(!h.barrier_open(&vcx));
}

/// Spec §7: a query error is a standing danger notice with the error
/// text; the last picture stays until a delivery replaces it, and `c`,
/// which rebuilds the pivot, does not hide it; the notice is dismissable
/// and returns as news.
#[gpui::test]
fn a_query_error_is_a_standing_notice_and_keeps_the_picture(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, "vega_kt", None);
    h.show_picture(&mut vcx);
    h.publish(&mut vcx, "vega_buckets");
    let reqs = h.requests();
    let tag = distincts(&reqs)[0].tag;
    h.answer_distinct(&mut vcx, tag, "tenor", &["1Y"]);
    h.answer_distinct(&mut vcx, tag, "moneyness", &["90.0"]);
    let _ = h.requests();
    h.fail_query(&mut vcx, tag, "'model_code' is not a column");
    assert_eq!(h.notices(&vcx), ["'model_code' is not a column"]);
    assert_eq!(
        h.delegate(&vcx, |d| d.layout.data_rows),
        4,
        "the picture stays"
    );
    vcx.simulate_keystrokes("c");
    assert_eq!(
        h.notices(&vcx),
        ["'model_code' is not a column"],
        "a rebuilt pivot leaves the error standing"
    );
    vcx.simulate_keystrokes("escape");
    assert!(h.tile.read_with(&vcx, |t, _| !t.dismissed.is_empty()));
    h.publish(&mut vcx, "vega_buckets");
    let reqs = h.requests();
    let tag = distincts(&reqs)[0].tag;
    h.answer_distinct(&mut vcx, tag, "tenor", &["1Y"]);
    h.answer_distinct(&mut vcx, tag, "moneyness", &["90.0"]);
    let _ = h.requests();
    h.fail_query(&mut vcx, tag, "'book' is not a column");
    assert!(
        h.tile
            .read_with(&vcx, |t, _| t.dismissed.visible(t.reported_notices()).len()
                == 1),
        "news is shown again"
    );
}

/// Spec §7: no generation yet is a status notice; an empty scoped
/// picture under a generation is an empty grid with no notice but the
/// domain still painted.
#[gpui::test]
fn no_generation_yet_is_a_status_notice_and_an_empty_scope_paints_the_domain(
    cx: &mut gpui::TestAppContext,
) {
    use crate::core::pivot::fixtures::Rows;
    let (h, mut vcx) = open_bound(cx, "vega_kt", None);
    h.show(&mut vcx);
    let reqs = h.requests();
    let tag = distincts(&reqs)[0].tag;
    h.answer_distinct(&mut vcx, tag, "tenor", &["1Y", "2Y"]);
    h.answer_distinct(&mut vcx, tag, "moneyness", &["90.0"]);
    let _ = h.requests();
    let empty = Rows {
        rows: vec![(vec![None, None], 0, vec![None, None])],
        ..vega_main()
    };
    let empty_totals = Rows {
        rows: vec![(vec![None], 0, vec![None, None])],
        ..vega_totals()
    };
    h.answer_query(&mut vcx, tag, empty.snapshot("vega_buckets", None));
    let _ = h.requests();
    h.answer_query(&mut vcx, tag, empty_totals.snapshot("vega_buckets", None));
    assert_eq!(h.notices(&vcx), ["no vega_buckets data yet"]);
    assert_eq!(
        h.delegate(&vcx, |d| (d.layout.data_rows, d.layout.data_cols)),
        (2, 1),
        "the domain paints"
    );
    // Now under a generation: an uncovered scope is an empty grid, no notice.
    h.publish(&mut vcx, "vega_buckets");
    let reqs = h.requests();
    let tag = distincts(&reqs)[0].tag;
    h.answer_distinct(&mut vcx, tag, "tenor", &["1Y", "2Y"]);
    h.answer_distinct(&mut vcx, tag, "moneyness", &["90.0"]);
    let _ = h.requests();
    h.answer_query(&mut vcx, tag, empty.snapshot("vega_buckets", Some(2)));
    let _ = h.requests();
    h.answer_query(
        &mut vcx,
        tag,
        empty_totals.snapshot("vega_buckets", Some(2)),
    );
    assert!(h.notices(&vcx).is_empty(), "{:?}", h.notices(&vcx));
    assert_eq!(h.delegate(&vcx, |d| d.layout.data_rows), 2);
}

/// Spec §7: diff without a base row in scope shows absolutes and a
/// standing notice naming the axis and base.
#[gpui::test]
fn a_missing_diff_base_is_a_standing_notice(cx: &mut gpui::TestAppContext) {
    use crate::core::pivot::fixtures::Rows;
    let (h, mut vcx) = open_bound_with(cx, crate::core::pivot::fixtures::spot_ladder(), None);
    h.show(&mut vcx);
    let reqs = h.requests();
    let tag = distincts(&reqs)[0].tag;
    h.answer_distinct(&mut vcx, tag, "spot_shift", &["-5.0", "0.0", "5.0"]);
    let _ = h.requests();
    let without_base = Rows {
        rows: ladder_main()
            .rows
            .into_iter()
            .filter(|(l, _, _)| l != &[Some("0.0")])
            .collect(),
        ..ladder_main()
    };
    h.answer_query(&mut vcx, tag, without_base.snapshot("spot_ladder", Some(1)));
    assert_eq!(
        h.notices(&vcx),
        ["diff: no base row (spot_shift = 0) in scope"]
    );
    assert_eq!(
        h.delegate(&vcx, |d| d.pivot.as_ref().unwrap().base_row),
        None
    );
    vcx.simulate_keystrokes("m");
    assert!(
        h.notices(&vcx).is_empty(),
        "absolute mode has no missing base"
    );
}

/// A launch record narrows the tile to the row's underlying: the first
/// main query carries `underlying_ref = SPX` inside the frame scope.
#[gpui::test]
fn a_launch_record_narrows_to_the_underlying(cx: &mut gpui::TestAppContext) {
    let mut dims = toml::Table::new();
    dims.insert(
        "underlying_ref".into(),
        toml::Value::Array(vec![toml::Value::String("SPX".into())]),
    );
    let mut filter = toml::Table::new();
    filter.insert("dimensions".into(), toml::Value::Table(dims));
    let mut record = toml::Table::new();
    record.insert("filter".into(), toml::Value::Table(filter));
    let (h, mut vcx) = open_bound(cx, "vega_kt", Some(record));
    h.show(&mut vcx);
    let reqs = h.requests();
    assert!(
        distincts(&reqs)[0].scope.is_empty(),
        "the domains are unscoped even under a filter layer"
    );
    let tag = distincts(&reqs)[0].tag;
    h.answer_distinct(&mut vcx, tag, "tenor", &["1Y"]);
    h.answer_distinct(&mut vcx, tag, "moneyness", &["90.0"]);
    let reqs = h.requests();
    assert_eq!(queries(&reqs)[0].scope, underlying_scope("SPX"));
    assert!(h.title(&mut vcx).ends_with("· SPX"));
    vcx.update(|w, cx| h.content.command("filter clear", w, cx))
        .unwrap();
    vcx.run_until_parked();
    let reqs = h.requests();
    assert!(
        queries(&reqs)[0].scope.is_empty(),
        "the layer cleared: {reqs:?}"
    );
}

#[gpui::test]
fn the_session_round_trips_through_the_factory(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, "spot_ladder", None);
    vcx.update(|w, cx| h.content.command("unscoped", w, cx))
        .unwrap();
    vcx.update(|w, cx| h.content.command("scale m", w, cx))
        .unwrap();
    vcx.simulate_keystrokes("c p m");
    let t = vcx.update(|_, cx| h.content.serialize(cx));
    let (restored, mut vcx2) = open_bound(cx, "spot_ladder", Some(t.clone()));
    let s = restored.state(&vcx2);
    assert!(s.unscoped && !s.diff);
    assert_eq!(s.scale, Some(geode_core::view::Scale::Millions));
    assert_eq!(s.currency, crate::core::pivot::Currency::Native);
    assert_eq!(s.paint, geode_core::scenario::Paint::Plain);
    assert_eq!(vcx2.update(|_, cx| restored.content.serialize(cx)), t);
}

/// A native sum over mixed currencies: the picture says so and the chip
/// (Task 9) reads it from `currency_mixed`.
#[gpui::test]
fn a_mixed_native_picture_is_flagged(cx: &mut gpui::TestAppContext) {
    use crate::core::pivot::fixtures::{dim, sum};
    use geode_core::snapshot::{ColumnMeta, TestColumn};
    let (h, mut vcx) = open_bound(cx, "spot_ladder", None);
    h.show(&mut vcx);
    let reqs = h.requests();
    let tag = distincts(&reqs)[0].tag;
    h.answer_distinct(&mut vcx, tag, "spot_shift", &["0.0"]);
    let _ = h.requests();
    let snapshot = Snapshot::for_tests_with_provenance(
        vec![
            (
                dim("spot_shift", 1),
                TestColumn::Str(vec![Some("0.0"), None]),
            ),
            (dim("row_depth", 1), TestColumn::I32(vec![1, 0])),
            (sum("npv", 1), TestColumn::F64(vec![Some(1.0), None])),
            (sum("npv_usd", 1), TestColumn::F64(vec![Some(1.1), None])),
            (sum("delta", 1), TestColumn::F64(vec![Some(1.0), None])),
            (sum("delta_usd", 1), TestColumn::F64(vec![Some(1.1), None])),
            (sum("gamma", 1), TestColumn::F64(vec![Some(1.0), None])),
            (sum("gamma_usd", 1), TestColumn::F64(vec![Some(1.1), None])),
            (sum("vega", 1), TestColumn::F64(vec![Some(1.0), None])),
            (sum("vega_usd", 1), TestColumn::F64(vec![Some(1.1), None])),
            (sum("theta", 1), TestColumn::F64(vec![Some(1.0), None])),
            (sum("theta_usd", 1), TestColumn::F64(vec![Some(1.1), None])),
            (
                ColumnMeta {
                    mixed_flag: Some(13),
                    ..dim("currency", 1)
                },
                TestColumn::Str(vec![None, None]),
            ),
            (
                dim("currency#mixed", 1),
                TestColumn::Bool(vec![Some(true), Some(true)]),
            ),
        ],
        1,
        crate::core::pivot::fixtures::provenance("spot_ladder", Some(1)),
    );
    h.answer_query(&mut vcx, tag, snapshot);
    assert!(h.delegate(&vcx, |d| d.pivot.as_ref().unwrap().currency_mixed));
}
