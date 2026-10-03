//! Keys, `:` commands and the two choosers. Every key goes through the real
//! keymap (`simulate_keystrokes` under the shell stand-in) and every command
//! through `TileContent::command`.

use super::*;
use crate::core::build::x_axis;
use crate::core::model::{Kind, Pair};
use geode_core::document::join_key;
use geode_core::query::{CatalogSnapshot, DatasetCatalog, PartitionCatalog};
use geode_core::vol::{Coordinate, Grid, SliceRequest};

/// A tile on SPX.Z with both documents installed and its first batch
/// answered: the strip is 2026-10-16 (cvi), 2026-11-20 (chain), 2026-12-18
/// (cvi), 2027-03-19 (cvi), 2027-06-18 (chain), the first row active.
fn loaded(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    let (h, mut vcx) = open_on(cx, launched_on("SPX.Z"));
    h.show(&mut vcx);
    let (doc, chains) = published();
    let reqs = h.answer_documents(&mut vcx, &doc, &chains);
    let first = vols(&reqs)[0].clone();
    h.answer_vol(&mut vcx, &first);
    (h, vcx)
}

impl Harness {
    /// The one batch the last keys submitted, answered; `None` when none
    /// went out.
    pub(super) fn answer_last(&self, vcx: &mut gpui::VisualTestContext) -> Option<VolSliceParams> {
        let reqs = self.requests();
        let last = vols(&reqs).last().map(|p| (*p).clone())?;
        self.answer_vol(vcx, &last);
        Some(last)
    }
    pub(super) fn command(
        &self,
        vcx: &mut gpui::VisualTestContext,
        line: &str,
    ) -> Result<(), String> {
        let r = vcx.update(|window, cx| self.content.command(line, window, cx));
        vcx.run_until_parked();
        r
    }
    pub(super) fn state(&self, vcx: &gpui::VisualTestContext) -> State {
        self.tile.read_with(vcx, |t, _| t.state().clone())
    }
    pub(super) fn mode(&self, vcx: &gpui::VisualTestContext) -> String {
        self.tile.read_with(vcx, |t, _| {
            t.key_context().get("mode").unwrap_or_default().to_string()
        })
    }
    pub(super) fn tilelist(&self, vcx: &gpui::VisualTestContext) -> bool {
        self.tile
            .read_with(vcx, |t, _| t.key_context().has_flag("tilelist"))
    }
    /// The open chooser's rows and its highlighted row, or `None`.
    pub(super) fn chooser(&self, vcx: &gpui::VisualTestContext) -> Option<(Vec<String>, usize)> {
        self.tile.read_with(vcx, |t, _| t.chooser_rows())
    }
    /// The open chooser's ticked pairs, or `None`.
    pub(super) fn chooser_ticks(&self, vcx: &gpui::VisualTestContext) -> Option<Vec<Pair>> {
        self.tile.read_with(vcx, |t, _| t.chooser_ticks())
    }
    pub(super) fn draw(&self, vcx: &mut gpui::VisualTestContext) {
        vcx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        vcx.run_until_parked();
    }
}

fn slices(p: &VolSliceParams) -> Vec<&SliceRequest> {
    p.jobs
        .iter()
        .filter_map(|j| match j {
            VolJob::Slice { request, .. } => Some(request),
            _ => None,
        })
        .collect()
}

fn maps(p: &VolSliceParams) -> usize {
    p.jobs
        .iter()
        .filter(|j| matches!(j, VolJob::Map(_)))
        .count()
}

#[gpui::test]
fn jk_move_the_cursor_and_enter_solos_through_the_keymap(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    vcx.simulate_keystrokes("j j");
    assert_eq!(h.state(&vcx).cursor, 2);
    assert!(h.requests().is_empty(), "moving the cursor asks nothing");
    vcx.simulate_keystrokes("enter");
    h.answer_last(&mut vcx).expect("enter solos and resubmits");
    assert_eq!(h.labels(&vcx), vec!["cvi 2026-12-18".to_string()]);
    vcx.simulate_keystrokes("k down down down down");
    assert_eq!(h.state(&vcx).cursor, 4, "the arrows too, clamped");
    vcx.simulate_keystrokes("up");
    assert_eq!(h.state(&vcx).cursor, 3);
    assert_eq!(
        h.dispatched(&vcx)[..3],
        [
            "volslice::strip_down",
            "volslice::strip_down",
            "volslice::solo"
        ]
    );
}

/// The active set, as dates.
fn active(h: &Harness, vcx: &gpui::VisualTestContext) -> Vec<String> {
    h.state(vcx)
        .active
        .iter()
        .flatten()
        .map(|e| e.to_string())
        .collect()
}

/// `space` solos the cursor's row, as `enter` does; `ctrl+space` and
/// `shift+space` add it or take it out, and on the only active row they
/// are refused and nothing goes out.
#[gpui::test]
fn space_solos_and_ctrl_or_shift_space_adds_or_removes(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    vcx.simulate_keystrokes("ctrl-space");
    assert_eq!(
        active(&h, &vcx),
        ["2026-10-16"],
        "the last active row stays"
    );
    vcx.simulate_keystrokes("shift-space");
    assert_eq!(active(&h, &vcx), ["2026-10-16"]);
    assert!(h.requests().is_empty(), "a refused toggle asks nothing");
    vcx.simulate_keystrokes("j ctrl-space");
    assert_eq!(active(&h, &vcx), ["2026-10-16", "2026-11-20"]);
    h.answer_last(&mut vcx).expect("an add resubmits");
    vcx.simulate_keystrokes("j shift-space");
    assert_eq!(active(&h, &vcx), ["2026-10-16", "2026-11-20", "2026-12-18"]);
    h.answer_last(&mut vcx).expect("an add resubmits");
    vcx.simulate_keystrokes("k k shift-space");
    assert_eq!(
        active(&h, &vcx),
        ["2026-11-20", "2026-12-18"],
        "on an active row it takes it out"
    );
    h.answer_last(&mut vcx).expect("a removal resubmits");
    vcx.simulate_keystrokes("j j space");
    assert_eq!(active(&h, &vcx), ["2026-12-18"], "space solos");
    h.answer_last(&mut vcx).expect("a solo resubmits");
    assert_eq!(
        h.dispatched(&vcx),
        [
            "volslice::toggle_expiry",
            "volslice::toggle_expiry",
            "volslice::strip_down",
            "volslice::toggle_expiry",
            "volslice::strip_down",
            "volslice::toggle_expiry",
            "volslice::strip_up",
            "volslice::strip_up",
            "volslice::toggle_expiry",
            "volslice::strip_down",
            "volslice::strip_down",
            "volslice::solo",
        ]
    );
}

/// A kind digit hides that kind: its own jobs leave the batch.
#[gpui::test]
fn a_digit_toggles_a_kind_omitting_its_jobs(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    vcx.simulate_keystrokes("j ctrl-space");
    let p = h.answer_last(&mut vcx).expect("a toggle resubmits");
    assert!(maps(&p) > 0, "the chain at 2026-11-20 is asked");
    assert_eq!(
        h.labels(&vcx),
        ["cvi 2026-10-16", "cvi 2026-11-20", "chain 2026-11-20"]
    );
    vcx.simulate_keystrokes("3");
    let p = h.answer_last(&mut vcx).expect("a kind toggle resubmits");
    assert_eq!(maps(&p), 0, "the hidden chain asks no coordinates");
    assert!(h.state(&vcx).hidden.contains(&Kind::Chain));
    // A digit past the kinds does nothing; one naming an unloaded kind
    // still records the choice for when it loads.
    vcx.simulate_keystrokes("9");
    assert!(h.requests().is_empty());
    vcx.simulate_keystrokes("2");
    assert!(h.state(&vcx).hidden.contains(&Kind::Draft));
}

/// From moneyness, two steps reach delta, whose axis runs right to left.
#[gpui::test]
fn x_steps_to_delta_and_the_axis_reads_reversed(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    assert_eq!(h.state(&vcx).coordinate, Coordinate::Moneyness);
    vcx.simulate_keystrokes("x x");
    assert_eq!(h.state(&vcx).coordinate, Coordinate::Delta);
    let p = h
        .answer_last(&mut vcx)
        .expect("a coordinate change resubmits");
    assert!(slices(&p).iter().all(|r| r.coordinate == Coordinate::Delta));
    let x = h.tile.read_with(&vcx, |t, _| t.model().x);
    assert_eq!(x, x_axis(Coordinate::Delta));
    assert!(x.reversed);
}

/// `shift+d` is the tile's density toggle, over the workspace's duplicate.
#[gpui::test]
fn shift_d_requests_densities(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    vcx.simulate_keystrokes("shift-d");
    assert_eq!(h.dispatched(&vcx), ["volslice::density"]);
    let p = h.answer_last(&mut vcx).expect("densities resubmit");
    assert!(slices(&p).iter().all(|r| r.density), "{:?}", slices(&p));
    vcx.simulate_keystrokes("shift-d");
    let p = h.answer_last(&mut vcx).unwrap();
    assert!(slices(&p).iter().all(|r| !r.density));
}

/// `d` opens the chooser over every loaded pair, a fieldless list the
/// shell's `j`/`k` step; with nothing ticked `enter` shows the highlighted
/// pair and its jobs ride the next batch. Reopened, the highlight starts
/// on the pair shown and the pair is ticked.
#[gpui::test]
fn d_chooses_a_pair_and_the_batch_carries_its_diff_jobs(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    let cvi_chain = Pair::new(Kind::Cvi, Kind::Chain).unwrap();
    let chain_cvi = cvi_chain.reverse();
    vcx.simulate_keystrokes("d");
    assert_eq!(h.mode(&vcx), "menu");
    assert!(h.tilelist(&vcx), "the shared list steps reach it");
    assert_eq!(
        h.chooser(&vcx),
        Some((
            vec![
                "cvi \u{2212} chain".to_string(),
                "chain \u{2212} cvi".to_string()
            ],
            0
        ))
    );
    vcx.simulate_keystrokes("j");
    assert_eq!(h.chooser(&vcx).unwrap().1, 1, "j reached the list");
    assert_eq!(h.state(&vcx).cursor, 0, "and not the strip");
    vcx.simulate_keystrokes("k enter");
    assert_eq!(h.chooser(&vcx), None);
    assert_eq!(h.mode(&vcx), "normal");
    assert_eq!(h.state(&vcx).diffs, [cvi_chain]);
    // The front expiry has no chain: make one that does the active row.
    let _ = h.requests();
    vcx.simulate_keystrokes("j enter");
    let p = h.answer_last(&mut vcx).expect("the pair rides the batch");
    assert!(
        slices(&p)
            .iter()
            .any(|r| matches!(r.grid, Grid::At(_)) && !r.density),
        "the cvi evaluated at the chain's strikes: {:?}",
        p.jobs
    );
    vcx.simulate_keystrokes("d");
    assert_eq!(h.chooser(&vcx).unwrap().1, 0, "on the pair shown");
    assert_eq!(h.chooser_ticks(&vcx), Some(vec![cvi_chain]));
    // `space` on the reverse ticks it and unticks the pair; `escape`
    // discards the ticks.
    vcx.simulate_keystrokes("j space");
    assert_eq!(h.chooser_ticks(&vcx), Some(vec![chain_cvi]));
    assert_eq!(
        h.dispatched(&vcx).last().map(String::as_str),
        Some("volslice::tick"),
        "space is the chooser's while it is up"
    );
    vcx.simulate_keystrokes("escape");
    assert_eq!(h.chooser(&vcx), None);
    assert_eq!(h.state(&vcx).diffs, [cvi_chain]);
    // Unticking the only pair and applying shows none: the ticks were
    // touched, so the highlight is not a fallback.
    vcx.simulate_keystrokes("d space enter");
    assert_eq!(h.state(&vcx).diffs, []);
    // `ctrl+x` unticks everything and counts as a touch: `enter` then
    // shows none rather than falling back to the highlight.
    h.command(&mut vcx, "diff cvi - chain").unwrap();
    vcx.simulate_keystrokes("d ctrl-x");
    assert_eq!(h.chooser_ticks(&vcx), Some(vec![]));
    vcx.simulate_keystrokes("enter");
    assert_eq!(h.state(&vcx).diffs, []);
    vcx.simulate_keystrokes("d ctrl-x enter");
    assert_eq!(
        h.state(&vcx).diffs,
        [],
        "on an empty set too: no fallback to the highlight"
    );
    assert!(
        h.dispatched(&vcx)
            .contains(&"volslice::clear_ticks".to_string())
    );
}

/// Several pairs at once, through the keys: each `space` ticks a pair in
/// turn and `enter` shows them all in tick order; the batch carries each
/// pair's evaluation, the shared chain map once.
#[gpui::test]
fn the_chooser_ticks_several_pairs_and_the_batch_asks_each(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, None);
    h.show(&mut vcx);
    h.follow_a(&mut vcx);
    let draft = cvi(&TERMS, Some(("2026-10-16", 0.01)));
    h.post(&mut vcx, draft_of("SPX.Z", &draft, DraftMark::Editing));
    let (doc, _) = published();
    let reqs = h.answer_documents(&mut vcx, &doc, &[chain("2026-10-16")]);
    let first = vols(&reqs).last().map(|p| (*p).clone()).expect("a batch");
    h.answer_vol(&mut vcx, &first);
    // cvi - cvi draft, cvi - chain, cvi draft - cvi, cvi draft - chain, ...
    vcx.simulate_keystrokes("d j space j j space k k k space enter");
    let pairs = vec![
        Pair::new(Kind::Cvi, Kind::Chain).unwrap(),
        Pair::new(Kind::Draft, Kind::Chain).unwrap(),
        Pair::new(Kind::Cvi, Kind::Draft).unwrap(),
    ];
    assert_eq!(h.state(&vcx).diffs, pairs);
    let p = h.answer_last(&mut vcx).expect("the pairs ride the batch");
    assert_eq!(maps(&p), 1, "one chain map for both chain pairs");
    let at_chain = slices(&p)
        .iter()
        .filter(|r| matches!(r.grid, Grid::At(_)))
        .count();
    let at_curve = slices(&p)
        .iter()
        .filter(|r| matches!(r.grid, Grid::Job(_)))
        .count();
    assert_eq!((at_chain, at_curve), (2, 1), "{:?}", p.jobs);
    let lower: Vec<String> = h.tile.read_with(&vcx, |t, _| {
        t.model()
            .slots
            .iter()
            .filter(|s| s.axis == geode_chart::Axis::BottomLeft)
            .map(|s| s.label.to_string())
            .collect()
    });
    assert_eq!(
        lower,
        pairs
            .iter()
            .map(|p| format!("{} 2026-10-16", p.label()))
            .collect::<Vec<_>>(),
        "in tick order"
    );
}

fn partitions(batches: &[&str]) -> Vec<PartitionCatalog> {
    batches
        .iter()
        .map(|b| PartitionCatalog {
            batch: (*b).to_string(),
            ..PartitionCatalog::default()
        })
        .collect()
}

/// The catalog the picker lists: the first key part of both datasets'
/// partitions, sorted and deduplicated.
fn catalog() -> CatalogSnapshot {
    let chain_key = |u: &str| join_key(&[u.to_string(), "2026-11-20".to_string()]);
    let (spx, rut) = (chain_key("SPX.Z"), chain_key("RUT.Z"));
    CatalogSnapshot {
        datasets: vec![
            DatasetCatalog {
                name: CVI.into(),
                partitions: partitions(&["SPX.Z", "NDX.Z"]),
                ..DatasetCatalog::default()
            },
            DatasetCatalog {
                name: CHAIN.into(),
                partitions: partitions(&[&spx, &rut]),
                ..DatasetCatalog::default()
            },
            DatasetCatalog {
                name: "risk".into(),
                partitions: partitions(&["EOD"]),
                ..DatasetCatalog::default()
            },
        ],
        ..CatalogSnapshot::default()
    }
}

#[gpui::test]
fn u_picks_an_underlying_from_the_catalog_and_requeries(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.show(&mut vcx);
    let _ = h.requests();
    vcx.simulate_keystrokes("u");
    assert!(
        h.diagnostics
            .read_with(&vcx, |d, _| d.pending_catalog_request()),
        "opening asks for a fresh catalog"
    );
    assert_eq!(h.mode(&vcx), "insert");
    assert!(!h.tilelist(&vcx), "the field types j and k");
    assert_eq!(h.chooser(&vcx), Some((Vec::new(), 0)), "nothing known yet");
    // The catalog lands while the picker is open.
    h.diagnostics.update(&mut vcx, |d, cx| {
        d.catalog = Some(catalog());
        cx.notify();
    });
    vcx.run_until_parked();
    assert_eq!(
        h.chooser(&vcx).unwrap().0,
        vec!["NDX.Z", "RUT.Z", "SPX.Z"],
        "both datasets' first key parts, sorted, once each"
    );
    h.draw(&mut vcx);
    assert!(vcx.update(|w, cx| h.content.holds_focus(w, cx)));
    vcx.simulate_input("spx");
    vcx.simulate_keystrokes("enter");
    assert_eq!(h.chooser(&vcx), None);
    assert!(!vcx.update(|w, cx| h.content.holds_focus(w, cx)));
    assert_eq!(h.state(&vcx).underlying.as_deref(), Some("SPX.Z"));
    let reqs = h.requests();
    let asked = docs(&reqs);
    assert_eq!(asked.len(), 1, "{reqs:?}");
    assert_eq!(asked[0].document_key, vec!["SPX.Z".to_string()]);
    // Escape leaves the picker with nothing changed.
    vcx.simulate_keystrokes("u");
    h.draw(&mut vcx);
    vcx.simulate_keystrokes("escape");
    assert_eq!(h.chooser(&vcx), None);
    assert_eq!(h.mode(&vcx), "normal");
    assert!(h.requests().is_empty());
}

/// With the picker open, a bare `j` is text for the field, not the strip's
/// step and not a list step; the arrows step the list through the tile's
/// own insert bindings.
#[gpui::test]
fn the_picker_types_j_and_steps_with_the_arrows(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    h.diagnostics.update(&mut vcx, |d, cx| {
        d.catalog = Some(catalog());
        cx.notify();
    });
    vcx.run_until_parked();
    vcx.simulate_keystrokes("u");
    h.draw(&mut vcx);
    assert_eq!(
        h.chooser(&vcx).unwrap(),
        (vec!["NDX.Z".into(), "RUT.Z".into(), "SPX.Z".into()], 0)
    );
    vcx.simulate_keystrokes("down");
    assert_eq!(h.chooser(&vcx).unwrap().1, 1, "down steps the list");
    vcx.simulate_keystrokes("down up");
    assert_eq!(h.chooser(&vcx).unwrap().1, 1, "and up steps it back");
    assert_eq!(
        h.dispatched(&vcx)[1..],
        [
            "volslice::list_down",
            "volslice::list_down",
            "volslice::list_up"
        ]
    );
    vcx.simulate_keystrokes("j");
    let (text, query) = vcx.update(|_, cx| h.tile.read(cx).picker_text(cx)).unwrap();
    assert_eq!(
        (text.as_str(), query.as_str()),
        ("j", "j"),
        "typed, and ranked by"
    );
    assert_eq!(h.state(&vcx).cursor, 0, "the strip did not move");
    assert_eq!(h.dispatched(&vcx).len(), 4, "j reached no binding");
}

/// A tile the shell launched with no underlying prompts at once; one
/// launched on an underlying does not.
#[gpui::test]
fn a_launched_tile_with_no_underlying_prompts_at_once(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    vcx.update(|window, cx| h.content.launched(window, cx));
    vcx.run_until_parked();
    assert_eq!(h.mode(&vcx), "insert", "the picker is up");
    let (h2, mut vcx2) = open_on(cx, launched_on("SPX.Z"));
    vcx2.update(|window, cx| h2.content.launched(window, cx));
    vcx2.run_until_parked();
    assert_eq!(h2.mode(&vcx2), "normal");
}

#[gpui::test]
fn commands_set_the_underlying_coordinate_and_pair_and_refuse_bad_input(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = loaded(cx);
    h.command(&mut vcx, "x strike").unwrap();
    assert_eq!(h.state(&vcx).coordinate, Coordinate::Strike);
    let p = h.answer_last(&mut vcx).expect(":x resubmits");
    assert!(
        slices(&p)
            .iter()
            .all(|r| r.coordinate == Coordinate::Strike)
    );
    assert_eq!(
        h.command(&mut vcx, "diff cvi draft - cvi"),
        Err("cvi draft is not loaded".to_string())
    );
    assert_eq!(h.state(&vcx).diffs, []);
    let chain_cvi = Pair::new(Kind::Chain, Kind::Cvi).unwrap();
    h.command(&mut vcx, "diff chain - cvi").unwrap();
    assert_eq!(h.state(&vcx).diffs, [chain_cvi]);
    h.command(&mut vcx, "diff cvi - chain").unwrap();
    assert_eq!(
        h.state(&vcx).diffs,
        [chain_cvi.reverse()],
        "turning a pair on turns its reverse off"
    );
    h.command(&mut vcx, "diff cvi - chain").unwrap();
    assert_eq!(h.state(&vcx).diffs, [], "a second time turns it off");
    h.command(&mut vcx, "diff chain - cvi").unwrap();
    h.command(&mut vcx, "diff off").unwrap();
    assert_eq!(h.state(&vcx).diffs, []);
    assert!(h.command(&mut vcx, "x sideways").is_err());
    let _ = h.requests();
    h.command(&mut vcx, "underlying NDX.Z").unwrap();
    let reqs = h.requests();
    assert_eq!(docs(&reqs)[0].document_key, vec!["NDX.Z".to_string()]);
    assert_eq!(
        vcx.update(|_, cx| h.content.completions("diff ", 5, cx)),
        ["cvi", "chain", "off"],
        "the loaded kinds"
    );
}

/// A restored pair whose kind is not loaded says so, once for that pair,
/// while the others paint; `:diff` can still turn it off (turning on such
/// a pair is refused, turning one off never is).
#[gpui::test]
fn an_unloaded_pair_is_noticed_alone_and_can_be_turned_off(cx: &mut gpui::TestAppContext) {
    let mut restored = launched_on("SPX.Z");
    let session: toml::Table = r#"
        expiries = ["2026-11-20"]
        diffs = [["cvi draft", "cvi"], ["cvi", "chain"]]
    "#
    .parse()
    .unwrap();
    restored.extend(session);
    let (h, mut vcx) = open_on(cx, restored);
    h.show(&mut vcx);
    let (doc, chains) = published();
    let reqs = h.answer_documents(&mut vcx, &doc, &chains);
    let first = vols(&reqs)[0].clone();
    h.answer_vol(&mut vcx, &first);
    assert_eq!(
        h.notices(&vcx),
        ["diff cvi draft \u{2212} cvi: cvi draft is not loaded"]
    );
    assert!(
        h.labels(&vcx)
            .contains(&"cvi \u{2212} chain 2026-11-20".to_string()),
        "the loaded pair paints: {:?}",
        h.labels(&vcx)
    );
    h.command(&mut vcx, "diff cvi draft - cvi").unwrap();
    assert_eq!(
        h.state(&vcx).diffs,
        [Pair::new(Kind::Cvi, Kind::Chain).unwrap()]
    );
    h.answer_last(&mut vcx).expect("turning it off resubmits");
    assert!(h.notices(&vcx).is_empty(), "{:?}", h.notices(&vcx));
}

/// While following, the underlying is the group's: `:underlying` is
/// refused with the group's letter.
#[gpui::test]
fn underlying_is_refused_while_following(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, None);
    h.show(&mut vcx);
    h.follow_a(&mut vcx);
    assert_eq!(
        h.command(&mut vcx, "underlying SPX.Z"),
        Err("following A \u{2014} set the underlying there".to_string())
    );
    vcx.update(|window, cx| h.content.launched(window, cx));
    assert_eq!(h.mode(&vcx), "normal", "a follower is never prompted");
}

/// `[` and `]` step the split by a twentieth within the chart's clamp,
/// each step a new model version over the same slots.
#[gpui::test]
fn split_keys_step_the_split_as_a_new_model_version(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    let (v0, s0) = h
        .tile
        .read_with(&vcx, |t, _| (t.model().version, t.model().split));
    assert_eq!(s0, 0.7);
    vcx.simulate_keystrokes("]");
    let (v1, s1, n) = h.tile.read_with(&vcx, |t, _| {
        (t.model().version, t.model().split, t.model().slots.len())
    });
    assert_eq!((v1, s1, n), (v0 + 1, 0.75, 1));
    assert_eq!(h.state(&vcx).split, 0.75);
    assert!(h.requests().is_empty(), "a split asks nothing");
    vcx.simulate_keystrokes("] ] ]");
    assert_eq!(h.state(&vcx).split, 0.8, "clamped");
    vcx.simulate_keystrokes("[");
    assert_eq!(h.state(&vcx).split, 0.75);
}

/// The view keys move the view through the axis's scale and never touch
/// the model; `0` puts it back.
#[gpui::test]
fn view_keys_zoom_pan_and_reset(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    let full = h.tile.read_with(&vcx, |t, _| t.view()).unwrap();
    let version = h.version(&vcx);
    vcx.simulate_keystrokes("=");
    let zoomed = h.tile.read_with(&vcx, |t, _| t.view()).unwrap();
    assert!(zoomed.span() < full.span());
    assert!(
        ((zoomed.lo + zoomed.hi) - (full.lo + full.hi)).abs() < 1e-9,
        "about the centre"
    );
    vcx.simulate_keystrokes("l");
    let panned = h.tile.read_with(&vcx, |t, _| t.view()).unwrap();
    assert!(panned.lo > zoomed.lo, "l shows higher x on a forward axis");
    vcx.simulate_keystrokes("0");
    let reset = h.tile.read_with(&vcx, |t, _| t.view()).unwrap();
    assert_eq!((reset.lo, reset.hi), (full.lo, full.hi));
    assert_eq!(h.version(&vcx), version, "the model is untouched");
    assert_eq!(h.state(&vcx).view, Some((full.lo, full.hi)));
}

/// The tile's `shift+d` beats the workspace's duplicate inside the tile,
/// through the keymap the app builds.
#[test]
fn shift_d_resolves_to_density_over_the_workspace_duplicate() {
    use geode_shell::keymap::{MatchResult, Matcher, parse_keystroke};
    let (data, _rx) = DataHandle::for_tests();
    let keymap = app_keymap(&Rc::new(VolsliceFactory::new(data)));
    let stack = [
        KeyContext::new("workspace"),
        KeyContext::new("tile"),
        KeyContext::new(crate::KIND).pair("mode", "normal"),
    ];
    let ks = parse_keystroke("shift+d", geode_shell::defaults::default_mod()).unwrap();
    match Matcher::default().press(&keymap, ks.clone(), &stack) {
        MatchResult::Matched { action, .. } => assert_eq!(action.0, "volslice::density"),
        other => panic!("{other:?}"),
    }
    // Outside the tile, the same key is the workspace's.
    match Matcher::default().press(&keymap, ks, &stack[..2]) {
        MatchResult::Matched { action, .. } => assert_ne!(action.0, "volslice::density"),
        MatchResult::NoMatch => {}
        other => panic!("{other:?}"),
    }
}
