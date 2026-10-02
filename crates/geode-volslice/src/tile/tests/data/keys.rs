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

/// `space` adds a row; on the only active row it is refused and nothing
/// goes out. A kind digit hides that kind: its own jobs leave the batch.
#[gpui::test]
fn space_toggles_and_a_digit_toggles_a_kind_omitting_its_jobs(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    let before = h.state(&vcx).active;
    vcx.simulate_keystrokes("space");
    assert_eq!(h.state(&vcx).active, before, "the last active row stays");
    assert!(h.requests().is_empty(), "a refused toggle asks nothing");
    vcx.simulate_keystrokes("j space");
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

/// `d` opens the chooser over `none` and every loaded pair, a fieldless
/// list the shell's `j`/`k` step; `enter` sets the pair and its jobs ride
/// the next batch. Reopened, the highlight starts on the pair in force.
#[gpui::test]
fn d_chooses_a_pair_and_the_batch_carries_its_diff_jobs(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    vcx.simulate_keystrokes("d");
    assert_eq!(h.mode(&vcx), "menu");
    assert!(h.tilelist(&vcx), "the shared list steps reach it");
    assert_eq!(
        h.chooser(&vcx),
        Some((
            vec![
                "none".to_string(),
                "cvi \u{2212} chain".to_string(),
                "chain \u{2212} cvi".to_string()
            ],
            0
        ))
    );
    vcx.simulate_keystrokes("j");
    assert_eq!(h.chooser(&vcx).unwrap().1, 1, "j reached the list");
    assert_eq!(h.state(&vcx).cursor, 0, "and not the strip");
    // The front expiry has no chain: make one that does the active row.
    vcx.simulate_keystrokes("enter");
    assert_eq!(h.chooser(&vcx), None);
    assert_eq!(h.mode(&vcx), "normal");
    assert_eq!(h.state(&vcx).diff, Pair::new(Kind::Cvi, Kind::Chain));
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
    assert_eq!(h.chooser(&vcx).unwrap().1, 1, "on the pair in force");
    vcx.simulate_keystrokes("escape");
    assert_eq!(h.chooser(&vcx), None);
    assert_eq!(h.state(&vcx).diff, Pair::new(Kind::Cvi, Kind::Chain));
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
    assert_eq!(h.state(&vcx).diff, None);
    h.command(&mut vcx, "diff chain - cvi").unwrap();
    assert_eq!(h.state(&vcx).diff, Pair::new(Kind::Chain, Kind::Cvi));
    h.command(&mut vcx, "diff none").unwrap();
    assert_eq!(h.state(&vcx).diff, None);
    assert!(h.command(&mut vcx, "x sideways").is_err());
    let _ = h.requests();
    h.command(&mut vcx, "underlying NDX.Z").unwrap();
    let reqs = h.requests();
    assert_eq!(docs(&reqs)[0].document_key, vec!["NDX.Z".to_string()]);
    assert_eq!(
        vcx.update(|_, cx| h.content.completions("diff ", 5, cx)),
        ["cvi", "chain", "none"],
        "the loaded kinds"
    );
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
