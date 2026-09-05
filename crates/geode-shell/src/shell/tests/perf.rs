//! The perf overlay toggle and the frame histogram it reads.

use super::*;

/// Spec §7.4's debug overlay toggle, end to end through the real key
/// pipeline: `mod+shift+p` (alt is the test/default mod) dispatches
/// `perf::toggle_overlay`, which paints the readout panel; a second
/// press removes it. Bounds via the `perf-overlay` debug selector —
/// the same honest what-the-test-can-see contract as
/// `empty_workspace_paints_the_hint`.
#[gpui::test]
fn perf_overlay_toggles_via_the_bound_action(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("perf-overlay").is_none(),
        "the overlay must start hidden"
    );

    cx.simulate_keystrokes("alt-shift-p");
    assert!(
        shell.read_with(&cx, |shell, _| shell.perf_overlay),
        "alt+shift+p should dispatch perf::toggle_overlay and set the flag"
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let bounds = cx.debug_bounds("perf-overlay");
    assert!(
        bounds.is_some_and(|b| b.size.width > px(0.0) && b.size.height > px(0.0)),
        "the perf overlay should paint with non-zero bounds, got {bounds:?}"
    );

    cx.simulate_keystrokes("alt-shift-p");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("perf-overlay").is_none(),
        "a second toggle should remove the overlay"
    );
}

/// Spec §7's vertical slice, end to end through the real key pipeline:
/// `mod+shift+d` dispatches `data::toggle_probe`, a snapshot pushed in
/// by the binary paints, and the attribution rules are visible in what
/// painted. This is the only place the §7.1 budget's *painted frame*
/// half is exercised at all — the benchmarks stop at the snapshot.
#[gpui::test]
fn the_data_probe_paints_a_pushed_snapshot(cx: &mut gpui::TestAppContext) {
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};

    let (window, mut cx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("data-probe").is_none(),
        "the probe must start hidden"
    );

    // The §6.3 example: an additive measure and a coarse one, the
    // latter non-attributable at the depth below its own grain.
    let meta = |name: &str, by_depth: Vec<Attribution>| ColumnMeta {
        name: name.into(),
        attribution_by_depth: by_depth,
        scope_semantics: ScopeSemantics::Direct,
    };
    let snapshot = Snapshot::for_tests(
        vec![
            (
                meta("lhu", vec![Attribution::Additive; 2]),
                TestColumn::Str(vec![None, Some("LHU1")]),
            ),
            (
                meta("row_depth", vec![Attribution::Additive; 2]),
                TestColumn::I64(vec![0, 1]),
            ),
            (
                meta("delta01", vec![Attribution::Additive; 2]),
                TestColumn::F64(vec![Some(30.0), Some(30.0)]),
            ),
            (
                meta(
                    "daily_trading_pnl",
                    vec![Attribution::Additive, Attribution::NonAttributable],
                ),
                TestColumn::F64(vec![Some(7.0), Some(7.0)]),
            ),
        ],
        1,
    );
    shell.update(&mut cx, |shell, cx| {
        shell.set_probe(
            crate::dataprobe::ProbeState {
                snapshot: Some(std::sync::Arc::new(snapshot)),
                freshness: vec![("BK000".into(), "2026-08-30T14:32:00Z".into(), 47)],
                query_micros: 22_700,
                error: None,
            },
            cx,
        );
    });

    cx.simulate_keystrokes("alt-shift-d");
    assert!(
        shell.read_with(&cx, |shell, _| shell.data_probe_visible()),
        "alt+shift+d should dispatch data::toggle_probe"
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let bounds = cx.debug_bounds("data-probe");
    assert!(
        bounds.is_some_and(|b| b.size.width > px(0.0) && b.size.height > px(0.0)),
        "the probe should paint with non-zero bounds, got {bounds:?}"
    );

    cx.simulate_keystrokes("alt-shift-d");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("data-probe").is_none(),
        "a second toggle should remove the probe"
    );
}

/// The recording seam: every `ShellView::render` after the first
/// records one frame-interval sample (consecutive test draws are far
/// below `perf::IDLE_CUTOFF`), and `perf::reset` zeroes the counters
/// through the same dispatch chain every other action uses. Recording
/// itself must not notify — pinned here by the count being exactly
/// the number of draws driven, with no runaway extra frames.
#[gpui::test]
fn render_records_frame_samples_and_reset_clears_them(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut cx);
    // Window-open itself already drew at least once (the very first
    // render records nothing — no previous render to measure from —
    // but any second one records), so take the count after an
    // explicit draw as the baseline rather than assuming 0.
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let baseline = shell.read_with(&cx, |shell, _| shell.perf.count());

    // Dirty the view and draw: each re-render past the first records
    // a sample. (A notify can flush into its own automatic test draw
    // in addition to the explicit one, so this asserts growth per
    // round, not an exact per-draw delta.)
    let mut last = baseline;
    for _ in 0..3 {
        shell.update(&mut cx, |_, cx| cx.notify());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let count = shell.read_with(&cx, |shell, _| shell.perf.count());
        assert!(
            count > last,
            "a dirtied re-render should record at least one sample \
             (was {last}, now {count})"
        );
        last = count;
    }
    assert!(
        shell.read_with(&cx, |shell, _| shell.perf.max_micros()) > 0,
        "recorded samples should carry a real nonzero interval"
    );

    // Recording must not itself notify (it would turn the shell into a
    // permanent redraw loop): once effects settle, the count stays put.
    cx.run_until_parked();
    let settled = shell.read_with(&cx, |shell, _| shell.perf.count());
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.perf.count()),
        settled,
        "no further samples may appear without a real invalidation"
    );

    // `perf::reset` zeroes the counters through the same dispatch
    // chain every action uses. Asserted inside the update, before the
    // notify it issues flushes into a fresh (recorded) repaint.
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("perf::reset".to_string()), None, window, cx);
            assert_eq!(
                shell.perf.count(),
                0,
                "perf::reset should zero the histogram"
            );
        });
    });
}
