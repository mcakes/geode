//! The chrome: the title and readout, the chips and their tones, the
//! dataset time's stale tone, the two-level header's groups, the health
//! chip, the menu's rows and picks, the footer's readout, and a pointer
//! press on a cell through the table's own route.

use super::data::{
    distincts, mixed_ladder_snapshot, open_bound, open_bound_with, queries, scope_of,
};
use super::*;
use crate::core::pivot::fixtures::{Rows, ladder_main, vega_main, vega_totals};
use geode_shell::shell::chip::Tone;

fn chip_texts(h: &Harness, vcx: &gpui::VisualTestContext) -> Vec<String> {
    h.tile.read_with(vcx, |t, _| {
        t.chips().iter().map(|c| c.text.to_string()).collect()
    })
}

fn draw(vcx: &mut gpui::VisualTestContext) {
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

#[gpui::test]
fn the_title_is_the_shape_title_and_the_readout(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, "vega_kt", None);
    // The header's readout is prepared when the scope moves, never in
    // render: a frame post and a `:` line each re-prepare it.
    let readout =
        |vcx: &gpui::VisualTestContext| h.tile.read_with(vcx, |t, _| t.readout().1.to_string());
    assert_eq!(h.title(&mut vcx), "Vega K/T · all");
    assert_eq!(readout(&vcx), "all");
    h.show_picture(&mut vcx);
    h.follow_a(&mut vcx);
    let _ = h.requests();
    h.post(&mut vcx, scope_of("SPX"));
    assert_eq!(h.title(&mut vcx), "Vega K/T · SPX");
    assert_eq!(readout(&vcx), "SPX");
    vcx.update(|w, cx| h.content.command("filter vega > 0", w, cx))
        .unwrap();
    assert_eq!(h.title(&mut vcx), "Vega K/T · SPX · vega > 0");
    assert_eq!(readout(&vcx), "SPX · vega > 0");
    draw(&mut vcx);
    assert!(
        vcx.debug_bounds("scenario-readout-7").is_some(),
        "the header paints the readout"
    );
}

#[gpui::test]
fn chips_follow_the_state_in_spec_order(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, "spot_ladder", None);
    assert_eq!(chip_texts(&h, &vcx), ["USD", "diff"]);
    vcx.simulate_keystrokes("c");
    assert_eq!(chip_texts(&h, &vcx), ["native", "diff"]);
    for line in ["scale k", "filter text x", "unscoped", "asof live"] {
        vcx.update(|w, cx| h.content.command(line, w, cx)).unwrap();
    }
    assert_eq!(
        chip_texts(&h, &vcx),
        ["native", "k", "filtered", "diff", "unscoped", "LIVE"]
    );
    vcx.simulate_keystrokes("m");
    assert_eq!(
        chip_texts(&h, &vcx),
        ["native", "k", "filtered", "unscoped", "LIVE"]
    );
    let (tones, actions): (Vec<Tone>, Vec<Option<&str>>) = h.tile.read_with(&vcx, |t, _| {
        let chips = t.chips();
        (
            chips.iter().map(|c| c.tone).collect(),
            chips.iter().map(|c| c.action).collect(),
        )
    });
    assert_eq!(tones[3], Tone::Warning, "unscoped warns");
    assert_eq!(
        actions[0],
        Some("scenario::currency"),
        "the currency chip is clickable"
    );
    let filtered = h.tile.read_with(&vcx, |t, _| t.chips()[2].tip.clone());
    assert_eq!(
        filtered.as_ref(),
        "text \"x\"; :filter clear removes it",
        "the tip spells the filter"
    );
}

/// A click on the currency chip takes the `c` key's path.
#[gpui::test]
fn a_click_on_the_currency_chip_toggles_the_currency(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, "spot_ladder", None);
    draw(&mut vcx);
    let chip = vcx
        .debug_bounds("scenario-chip-currency-7")
        .expect("the currency chip is painted");
    vcx.simulate_click(chip.center(), gpui::Modifiers::none());
    assert_eq!(h.state(&vcx).currency, crate::core::pivot::Currency::Native);
    assert_eq!(chip_texts(&h, &vcx)[0], "native");
}

/// Spec §5.3 and d3: native over mixed currencies warns on the chip and
/// its tooltip says what is summed, from the moment the picture lands;
/// USD over the same rows does not.
#[gpui::test]
fn the_currency_chip_warns_over_a_mixed_native_sum(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, "spot_ladder", None);
    vcx.simulate_keystrokes("c");
    let before = h.tile.read_with(&vcx, |t, _| t.chips()[0].clone());
    assert_eq!(
        (before.text.as_ref(), before.tone),
        ("native", Tone::Neutral),
        "no picture yet: nothing is summed"
    );
    h.show(&mut vcx);
    let reqs = h.requests();
    let tag = distincts(&reqs)[0].tag;
    h.answer_distinct(&mut vcx, tag, "spot_shift", &["0.0"]);
    let _ = h.requests();
    h.answer_query(&mut vcx, tag, mixed_ladder_snapshot());
    let native = h.tile.read_with(&vcx, |t, _| t.chips()[0].clone());
    assert_eq!(
        (native.text.as_ref(), native.tone),
        ("native", Tone::Warning)
    );
    assert_eq!(native.tip.as_ref(), "native sums across currencies");
    vcx.simulate_keystrokes("c");
    let usd = h.tile.read_with(&vcx, |t, _| t.chips()[0].clone());
    assert_eq!((usd.text.as_ref(), usd.tone), ("USD", Tone::Neutral));
}

/// The dataset time takes the warning tone past `stale_after`, which the
/// factory's setting reaches on the next paint.
#[gpui::test]
fn the_dataset_time_warns_when_stale(cx: &mut gpui::TestAppContext) {
    use geode_core::snapshot::{Freshness, Provenance};
    let (h, mut vcx) = open_bound(cx, "vega_kt", None);
    h.show(&mut vcx);
    let reqs = h.requests();
    let tag = distincts(&reqs)[0].tag;
    h.answer_distinct(&mut vcx, tag, "tenor", &["1Y"]);
    h.answer_distinct(&mut vcx, tag, "moneyness", &["90.0"]);
    let _ = h.requests();
    let old = chrono::Utc::now() - chrono::Duration::hours(2);
    let snapshot = vega_main().snapshot_with(Provenance {
        datasets: vec![Freshness {
            dataset: "vega_buckets".into(),
            as_of: Some(old.to_rfc3339()),
            generation: Some(1),
        }],
        as_of_request: None,
    });
    h.answer_query(&mut vcx, tag, snapshot);
    let _ = h.requests();
    h.answer_query(
        &mut vcx,
        tag,
        vega_totals().snapshot("vega_buckets", Some(1)),
    );
    let runs = h
        .tile
        .read_with(&vcx, |t, _| t.time_runs(chrono::Utc::now()));
    assert_eq!(runs.len(), 1);
    assert!(
        runs[0].stale,
        "two hours old against a one-minute threshold"
    );
    assert!(runs[0].label.starts_with("vega_buckets "), "{runs:?}");
    h.factory.set_stale_after(Duration::from_secs(3 * 3600));
    let runs = h
        .tile
        .read_with(&vcx, |t, _| t.time_runs(chrono::Utc::now()));
    assert!(!runs[0].stale, "a reloaded threshold reaches the tile");
}

/// d7 / P3-n: the group row over a two-level header spans consecutive
/// columns by the outer axis value, in axis order, and the table paints it.
#[gpui::test]
fn the_band_spans_the_outer_axis_groups(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, "rho", None);
    h.show(&mut vcx);
    let reqs = h.requests();
    let tag = distincts(&reqs)[0].tag;
    h.answer_distinct(&mut vcx, tag, "tenor", &["1Y", "2Y"]);
    h.answer_distinct(&mut vcx, tag, "curve_ccy", &["EUR", "USD"]);
    h.answer_distinct(&mut vcx, tag, "curve_type", &["OIS", "RFR"]);
    let _ = h.requests();
    let main = Rows {
        grouping: vec!["tenor", "curve_ccy", "curve_type"],
        measures: vec!["rho", "rho_usd"],
        rows: vec![
            (
                vec![Some("1Y"), Some("USD"), Some("OIS")],
                3,
                vec![Some(1.0), Some(1.0)],
            ),
            (
                vec![Some("1Y"), Some("USD"), Some("RFR")],
                3,
                vec![Some(2.0), Some(2.0)],
            ),
            (
                vec![Some("1Y"), Some("EUR"), Some("OIS")],
                3,
                vec![Some(3.0), Some(3.3)],
            ),
            (
                vec![Some("2Y"), Some("EUR"), Some("RFR")],
                3,
                vec![Some(4.0), Some(4.4)],
            ),
            (
                vec![Some("1Y"), Some("USD"), None],
                2,
                vec![Some(3.0), Some(3.0)],
            ),
            (
                vec![Some("1Y"), Some("EUR"), None],
                2,
                vec![Some(3.0), Some(3.3)],
            ),
            (
                vec![Some("2Y"), Some("EUR"), None],
                2,
                vec![Some(4.0), Some(4.4)],
            ),
            (vec![Some("1Y"), None, None], 1, vec![Some(6.0), Some(6.3)]),
            (vec![Some("2Y"), None, None], 1, vec![Some(4.0), Some(4.4)]),
            (vec![None, None, None], 0, vec![Some(10.0), Some(10.7)]),
        ],
    };
    h.answer_query(&mut vcx, tag, main.snapshot("rho_curves", Some(1)));
    let reqs = h.requests();
    assert_eq!(
        queries(&reqs)[0].grouping.as_deref(),
        Some(&["curve_ccy".to_string(), "curve_type".to_string()][..])
    );
    let totals = Rows {
        grouping: vec!["curve_ccy", "curve_type"],
        measures: vec!["rho", "rho_usd"],
        rows: vec![
            (
                vec![Some("USD"), Some("OIS")],
                2,
                vec![Some(1.0), Some(1.0)],
            ),
            (
                vec![Some("USD"), Some("RFR")],
                2,
                vec![Some(2.0), Some(2.0)],
            ),
            (
                vec![Some("EUR"), Some("OIS")],
                2,
                vec![Some(3.0), Some(3.3)],
            ),
            (
                vec![Some("EUR"), Some("RFR")],
                2,
                vec![Some(4.0), Some(4.4)],
            ),
            (vec![Some("USD"), None], 1, vec![Some(3.0), Some(3.0)]),
            (vec![Some("EUR"), None], 1, vec![Some(7.0), Some(7.7)]),
            (vec![None, None], 0, vec![Some(10.0), Some(10.7)]),
        ],
    };
    h.answer_query(&mut vcx, tag, totals.snapshot("rho_curves", Some(1)));
    let groups = h.delegate(&vcx, |d| d.band_groups());
    assert_eq!(
        groups,
        [("EUR".to_string(), 2), ("USD".to_string(), 2)],
        "lexical outer order; pairs present: EUR›OIS, EUR›RFR, USD›OIS, USD›RFR"
    );
    draw(&mut vcx);
    assert!(
        vcx.debug_bounds("scenario-band-EUR").is_some()
            && vcx.debug_bounds("scenario-band-USD").is_some(),
        "the table paints the group row"
    );
    // A one-level header has no group row.
    let (v, mut vcx2) = open_bound(cx, "vega_kt", None);
    v.show_picture(&mut vcx2);
    assert!(v.delegate(&vcx2, |d| d.band_groups()).is_empty());
}

/// The health chip shows while a source of the shape's dataset is not ok.
#[gpui::test]
fn a_failing_source_of_the_dataset_shows_the_health_chip(cx: &mut gpui::TestAppContext) {
    use geode_shell::diagnostics::SourceSummary;
    let (h, mut vcx) = open_bound(cx, "vega_kt", None);
    assert!(h.tile.read_with(&vcx, |t, _| t.health.chip().is_none()));
    h.diagnostics.update(&mut vcx, |d, cx| {
        d.describe_source("vega_src", SourceSummary::for_dataset("vega_buckets"));
        d.note_health(
            "vega_src",
            geode_core::health::Health::Failed {
                reason: "torn".into(),
            },
            "torn".into(),
            std::time::SystemTime::UNIX_EPOCH,
        );
        cx.notify();
    });
    vcx.run_until_parked();
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, _| t.health.chip().map(|c| c.word().to_string())),
        Some("failed".to_string())
    );
}

#[gpui::test]
fn the_menu_opens_from_dot_lists_the_verbs_and_a_pick_dispatches(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, "spot_ladder", None);
    draw(&mut vcx);
    assert!(vcx.debug_bounds("scenario-menu-button-7").is_some());
    assert!(vcx.debug_bounds("scenario-menu-7").is_none());
    vcx.simulate_keystrokes(".");
    let (rows, highlighted) = h
        .tile
        .read_with(&vcx, |t, _| t.menu_open().expect("the menu is up"));
    assert_eq!(
        rows,
        [
            "scenario::visual_rows",
            "scenario::visual_block",
            "scenario::yank",
            "scenario::currency",
            "scenario::paint",
            "scenario::diff",
            "scenario::unscoped",
            "scenario::scale_k",
            "scenario::scale_m",
            "scenario::scale_1",
            "scenario::filter_clear"
        ]
    );
    assert_eq!(highlighted, Some(0));
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, cx| t
                .key_context(cx)
                .get("mode")
                .map(str::to_string))
            .as_deref(),
        Some("menu")
    );
    draw(&mut vcx);
    assert!(
        vcx.debug_bounds("scenario-menu-7").is_some(),
        "the menu is painted"
    );
    vcx.simulate_keystrokes("j j j enter");
    assert!(
        h.tile.read_with(&vcx, |t, _| t.menu_open().is_none()),
        "a pick closes the menu"
    );
    assert_eq!(
        h.state(&vcx).currency,
        crate::core::pivot::Currency::Native,
        "the pick dispatched `currency`"
    );
    vcx.simulate_keystrokes(".");
    assert!(h.tile.read_with(&vcx, |t, _| t.menu_open().is_some()));
    vcx.simulate_keystrokes("escape");
    assert!(h.tile.read_with(&vcx, |t, _| t.menu_open().is_none()));
    // `⋯` opens it by the pointer, and a press outside closes it.
    draw(&mut vcx);
    let button = vcx.debug_bounds("scenario-menu-button-7").unwrap();
    vcx.simulate_click(button.center(), gpui::Modifiers::none());
    assert!(h.tile.read_with(&vcx, |t, _| t.menu_open().is_some()));
    draw(&mut vcx);
    let footer = vcx.debug_bounds("scenario-footer-7").unwrap();
    vcx.simulate_click(footer.center(), gpui::Modifiers::none());
    assert!(h.tile.read_with(&vcx, |t, _| t.menu_open().is_none()));
    let (h2, mut vcx2) = open_bound(cx, "vega_kt", None);
    vcx2.simulate_keystrokes(".");
    let ticks = h2.tile.read_with(&vcx2, |t, _| {
        (
            t.menu_enabled("scenario::diff"),
            t.menu_tick("scenario::paint"),
        )
    });
    assert_eq!(
        ticks,
        (Some(false), Some(Some(true))),
        "diff disabled on a matrix; heat ticked"
    );
}

/// A `:` command row runs its line, and an open menu's ticks follow it.
#[gpui::test]
fn a_command_row_runs_its_line_and_the_ticks_follow(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, "vega_kt", None);
    vcx.simulate_keystrokes(".");
    // Down to `:scale k`: the steps skip separators and the disabled
    // `diff` row.
    let lit = |vcx: &gpui::VisualTestContext| {
        h.tile.read_with(vcx, |t, _| {
            t.menu.as_ref().and_then(|m| {
                let i = m.menu.highlighted()?;
                m.menu.rows()[i].action().map(|r| r.pick().0.clone())
            })
        })
    };
    for _ in 0..12 {
        if lit(&vcx).as_deref() == Some("scenario::scale_k") {
            break;
        }
        vcx.simulate_keystrokes("j");
    }
    assert_eq!(lit(&vcx).as_deref(), Some("scenario::scale_k"));
    vcx.simulate_keystrokes("enter");
    assert_eq!(
        h.state(&vcx).scale,
        Some(geode_core::view::Scale::Thousands)
    );
    vcx.simulate_keystrokes(".");
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, _| t.menu_tick("scenario::scale_k")),
        Some(Some(true))
    );
    vcx.simulate_keystrokes("p");
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, _| t.menu_tick("scenario::paint")),
        Some(Some(true)),
        "a key while the menu is up does not reach the tile's verbs"
    );
    vcx.update(|w, cx| h.content.command("scale 1", w, cx))
        .unwrap();
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| (
            t.menu_tick("scenario::scale_k"),
            t.menu_tick("scenario::scale_1")
        )),
        (Some(Some(false)), Some(Some(true))),
        "a `:` line under the open menu re-ticks it"
    );
}

#[gpui::test]
fn the_footer_reads_the_cursor_cell_or_the_selection(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, "vega_kt", None);
    h.show_picture(&mut vcx);
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, cx| t.cursor_readout(cx))
            .as_deref(),
        Some("1Y × 90.0 · 12.00")
    );
    vcx.simulate_keystrokes("l");
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, cx| t.cursor_readout(cx))
            .as_deref(),
        Some("1Y × 100.0 · 24.00")
    );
    vcx.simulate_keystrokes("v j");
    let (extent, totals) = h.delegate(&vcx, |d| {
        (
            d.extent.clone(),
            d.summary
                .iter()
                .map(|c| c.text.to_string())
                .collect::<Vec<_>>(),
        )
    });
    assert_eq!(extent.as_deref(), Some("2 rows × 1 col"));
    assert_eq!(
        totals,
        ["30.00"],
        "24 + 6 down the 100.0 column (1Y and 18M)"
    );
    let (l, mut vcx2) = open_bound_with(cx, crate::core::pivot::fixtures::spot_ladder(), None);
    l.show(&mut vcx2);
    let reqs = l.requests();
    let tag = distincts(&reqs)[0].tag;
    l.answer_distinct(&mut vcx2, tag, "spot_shift", &["-5.0", "0.0", "5.0"]);
    let _ = l.requests();
    l.answer_query(
        &mut vcx2,
        tag,
        ladder_main().snapshot("spot_ladder", Some(1)),
    );
    vcx2.simulate_keystrokes("shift-v j");
    let totals = l.delegate(&vcx2, |d| {
        d.summary
            .iter()
            .map(|c| (c.text.to_string(), c.refused))
            .collect::<Vec<_>>()
    });
    assert_eq!(totals.len(), 3, "one total per measure column");
    assert!(
        totals
            .iter()
            .all(|(t, refused)| *refused && t.contains('‡')),
        "a scenario rows axis never sums: {totals:?}"
    );
}

/// A press on a cell reaches the tile through the table's own pointer
/// route (`wire_pointer` → `CellPointer` → the tile): it moves the cursor,
/// a shift press extends a block from it, and the footer reads the result.
#[gpui::test]
fn a_cell_press_moves_the_cursor_and_a_shift_press_extends_a_block(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, "vega_kt", None);
    h.show_picture(&mut vcx);
    draw(&mut vcx);
    let cell = vcx
        .debug_bounds("scenario-cell-0-2")
        .expect("1Y × 100.0 is painted");
    vcx.simulate_click(cell.center(), gpui::Modifiers::none());
    assert_eq!(h.delegate(&vcx, |d| d.cursor), (0, 1));
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, cx| t.cursor_readout(cx))
            .as_deref(),
        Some("1Y × 100.0 · 24.00")
    );
    draw(&mut vcx);
    let below = vcx
        .debug_bounds("scenario-cell-1-2")
        .expect("18M × 100.0 is painted");
    vcx.simulate_click(below.center(), gpui::Modifiers::shift());
    assert_eq!(
        h.delegate(&vcx, |d| (d.cursor, d.extent.clone())),
        ((1, 1), Some("2 rows × 1 col".into()))
    );
    // A press on the label column focuses only.
    draw(&mut vcx);
    let label = vcx
        .debug_bounds("scenario-cell-2-0")
        .expect("the 2Y label is painted");
    vcx.simulate_click(label.center(), gpui::Modifiers::none());
    assert_eq!(h.delegate(&vcx, |d| d.cursor), (1, 1));
}
