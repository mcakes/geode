//! Every key reaches its action through the production keymap under the
//! tile's live context, and the verbs change the state they own.

use super::*;
use crate::core::pivot::Currency;
use geode_core::scenario::Paint;

#[gpui::test]
fn every_scenario_key_reaches_its_action(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    for (keys, action) in [
        ("shift-v", "scenario::visual_rows"),
        ("escape", "scenario::escape"),
        ("v", "scenario::visual_block"),
        ("escape", "scenario::escape"),
        ("y", "scenario::yank"),
        ("c", "scenario::currency"),
        ("p", "scenario::paint"),
        ("m", "scenario::diff"),
        (".", "scenario::menu"),
        ("escape", "scenario::menu_close"),
        (".", "scenario::menu"),
        (".", "scenario::menu_close"),
    ] {
        vcx.simulate_keystrokes(keys);
        let last = h.dispatched(&vcx).last().cloned();
        assert_eq!(last.as_deref(), Some(action), "{keys}");
    }
    assert!(
        h.tile.read_with(&vcx, |t, _| t.menu.is_none()),
        "the menu closed"
    );
}

#[gpui::test]
fn ctrl_c_yanks_beside_y(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    vcx.simulate_keystrokes("ctrl-c");
    assert_eq!(
        h.dispatched(&vcx).last().map(|a| a.as_str()),
        Some("scenario::yank")
    );
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.refusal.clone()).as_deref(),
        Some("nothing to yank"),
        "no picture yet"
    );
}

#[gpui::test]
fn the_key_context_names_the_mode_and_the_selection_kind(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    let ctx = |vcx: &gpui::VisualTestContext| h.tile.read_with(vcx, |t, cx| t.key_context(cx));
    let c = ctx(&vcx);
    assert_eq!(c.get("mode"), Some("normal"));
    assert!(c.has_flag("grid") && c.has_flag("counts") && !c.has_flag("tilelist"));
    vcx.simulate_keystrokes("shift-v");
    let c = ctx(&vcx);
    assert_eq!(
        (c.get("mode"), c.get("select")),
        (Some("visual"), Some("rows"))
    );
    vcx.simulate_keystrokes("v");
    assert_eq!(ctx(&vcx).get("select"), Some("block"));
    vcx.simulate_keystrokes("v");
    assert_eq!(
        ctx(&vcx).get("mode"),
        Some("normal"),
        "v again ends the block"
    );
    vcx.simulate_keystrokes(".");
    let c = ctx(&vcx);
    assert_eq!(c.get("mode"), Some("menu"));
    assert!(c.has_flag("tilelist"));
}

#[gpui::test]
#[ignore = "needs the read chain's show_picture"]
fn grid_motions_move_the_cursor_cell_and_counts_multiply(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.show_picture(&mut vcx);
    vcx.simulate_keystrokes("j l");
    assert_eq!(h.delegate(&vcx, |d| d.cursor), (1, 1));
    vcx.simulate_keystrokes("2 k");
    assert_eq!(h.delegate(&vcx, |d| d.cursor), (0, 1));
    vcx.simulate_keystrokes("$");
    assert_eq!(
        h.delegate(&vcx, |d| d.cursor),
        (0, 2),
        "the last data column, never the Σ column"
    );
    vcx.simulate_keystrokes("shift-g");
    assert_eq!(
        h.delegate(&vcx, |d| d.cursor),
        (3, 2),
        "the last data row, never the Σ row"
    );
    vcx.simulate_keystrokes("g g ^");
    assert_eq!(h.delegate(&vcx, |d| d.cursor), (0, 0));
}

#[gpui::test]
fn c_toggles_currency_without_a_requery_and_refuses_without_a_twin(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    assert_eq!(h.state(&vcx).currency, Currency::Usd);
    let before = h.requests().len();
    vcx.simulate_keystrokes("c");
    assert_eq!(h.state(&vcx).currency, Currency::Native);
    assert_eq!(
        h.requests().len(),
        0,
        "no query: the snapshot carries both columns ({before} before)"
    );
    // The pivot fixture's by-maturity shape has no twins (the demo one has).
    let (h2, mut vcx2) = open_with_shape(cx, crate::core::pivot::fixtures::by_maturity());
    vcx2.simulate_keystrokes("c");
    assert_eq!(h2.state(&vcx2).currency, Currency::Native);
    let refusal = h2.tile.read_with(&vcx2, |t, _| t.refusal.clone());
    assert_eq!(refusal.as_deref(), Some("no USD twin for repo"));
}

#[gpui::test]
fn p_cycles_heat_and_plain_and_m_refuses_without_a_diff(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    assert_eq!(h.state(&vcx).paint, Paint::Heat);
    vcx.simulate_keystrokes("p");
    assert_eq!(h.state(&vcx).paint, Paint::Plain);
    assert!(!h.delegate(&vcx, |d| d.heat));
    vcx.simulate_keystrokes("p");
    assert_eq!(h.state(&vcx).paint, Paint::Heat);
    vcx.simulate_keystrokes("m");
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.refusal.clone()).as_deref(),
        Some("this shape has no diff")
    );
    let (l, mut vcx2) = open_shape(cx, "spot_ladder");
    assert!(l.state(&vcx2).diff, "a shape with diff starts in diff mode");
    vcx2.simulate_keystrokes("m");
    assert!(!l.state(&vcx2).diff);
}

/// `escape` peels one layer per press, the selection before the refusal,
/// and reports each peel as handled; with nothing to peel it is not
/// handled, so the shell's own `escape` runs.
#[gpui::test]
fn escape_peels_the_selection_then_the_refusal_then_reports_unhandled(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    let escape = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|w, cx| {
            h.content
                .dispatch(&ActionId("scenario::escape".into()), None, w, cx)
        })
    };
    vcx.simulate_keystrokes("v");
    assert!(h.delegate(&vcx, |d| d.selection.is_some()));
    assert!(escape(&mut vcx), "the selection peeled");
    assert!(h.delegate(&vcx, |d| d.selection.is_none()));
    vcx.simulate_keystrokes("m");
    assert!(h.tile.read_with(&vcx, |t, _| t.refusal.is_some()));
    assert!(escape(&mut vcx), "the refusal peeled");
    assert!(
        h.tile.read_with(&vcx, |t, _| t.refusal.is_none()),
        "escape dismissed the refusal"
    );
    assert!(!escape(&mut vcx), "nothing left to peel");
}

#[gpui::test]
#[ignore = "needs the read chain's show_picture"]
fn escape_peels_the_selection_then_the_notice(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.show_picture(&mut vcx);
    vcx.simulate_keystrokes("v l");
    assert!(h.delegate(&vcx, |d| d.selection.is_some()));
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, cx| t
                .key_context(cx)
                .get("mode")
                .map(str::to_string))
            .as_deref(),
        Some("visual")
    );
    vcx.simulate_keystrokes("escape");
    assert!(h.delegate(&vcx, |d| d.selection.is_none()));
    vcx.simulate_keystrokes("m");
    assert!(h.tile.read_with(&vcx, |t, _| t.refusal.is_some()));
    vcx.simulate_keystrokes("escape");
    assert!(
        h.tile.read_with(&vcx, |t, _| t.refusal.is_none()),
        "escape dismissed the refusal"
    );
}

#[gpui::test]
#[ignore = "needs the read chain's show_picture"]
fn yank_copies_the_cursor_cell_then_a_block_as_tsv(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.show_picture(&mut vcx);
    vcx.simulate_keystrokes("y");
    let one = vcx.read_from_clipboard().and_then(|i| i.text()).unwrap();
    assert_eq!(one, "tenor\t90.0\n1Y\t10\n");
    vcx.simulate_keystrokes("v j l y");
    let block = vcx.read_from_clipboard().and_then(|i| i.text()).unwrap();
    assert_eq!(block.lines().count(), 3, "{block}");
    assert!(block.starts_with("tenor\t90.0\t100.0\n"));
    assert!(
        h.delegate(&vcx, |d| d.selection.is_none()),
        "yank ends the selection"
    );
}

#[gpui::test]
fn commands_edit_the_tile_layer_and_refuse_frame_wide_verbs(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    let run = |vcx: &mut gpui::VisualTestContext, line: &str| {
        vcx.update(|w, cx| h.content.command(line, w, cx))
    };
    assert!(run(&mut vcx, "unscoped").is_ok());
    assert!(h.state(&vcx).unscoped);
    assert!(run(&mut vcx, "scale k").is_ok());
    assert_eq!(
        h.state(&vcx).scale,
        Some(geode_core::view::Scale::Thousands)
    );
    assert_eq!(
        h.delegate(&vcx, |d| d.scale),
        Some(geode_core::view::Scale::Thousands),
        "the scale reaches the prepared cells"
    );
    assert!(run(&mut vcx, "filter text SPX").is_ok());
    assert_eq!(h.state(&vcx).filter.text.as_deref(), Some("SPX"));
    assert!(run(&mut vcx, "filter clear").is_ok());
    assert!(h.state(&vcx).filter.is_empty());
    assert_eq!(
        run(&mut vcx, "scope book = A"),
        Err(crate::core::commands::REFUSED_SCOPE.to_string())
    );
    assert!(run(&mut vcx, "asof live").is_ok());
    assert_eq!(
        h.state(&vcx).as_of,
        crate::core::session::TileAsOf::Pinned(geode_core::query::AsOf::Live)
    );
    assert!(run(&mut vcx, "asof 2026-10-09 16:30").is_ok());
    assert!(matches!(
        h.state(&vcx).as_of,
        crate::core::session::TileAsOf::Pinned(geode_core::query::AsOf::At(_))
    ));
    assert!(run(&mut vcx, "asof not-a-time").is_err());
    assert!(run(&mut vcx, "asof clear").is_ok());
    assert_eq!(h.state(&vcx).as_of, crate::core::session::TileAsOf::Follow);
    let comps = vcx.update(|_, cx| h.content.completions("filter ", 7, cx));
    assert!(
        comps.contains(&"clear".to_string()) && comps.contains(&"vega".to_string()),
        "{comps:?}"
    );
}

/// The session record round-trips through the shell's door: what the tile
/// serializes is what a restored tile starts from.
#[gpui::test]
fn a_serialized_tile_restores_its_state(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    vcx.simulate_keystrokes("c p");
    let _ = vcx.update(|w, cx| h.content.command("scale m", w, cx));
    let record = vcx.update(|_, cx| h.content.serialize(cx));
    let (r, vcx2) = open_framed(
        cx,
        crate::core::spec::builtin_shape_checked(KIND),
        Some(record),
        |frame| FrameRef::new(frame, WorkspaceIx::FIRST),
    );
    assert_eq!(r.state(&vcx2), h.state(&vcx));
    assert!(
        !r.delegate(&vcx2, |d| d.heat),
        "plain paint reaches the delegate"
    );
}
