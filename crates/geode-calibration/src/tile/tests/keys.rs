//! The cursor keys, the stage jumps, the filter and the painted graph:
//! every key through the app's keymap, the result read off the prepared
//! model and the painted bounds.

use geode_shell::keymap::KeyContext;
use gpui::{px, size};

use super::harness::{Harness, builtin_config, open_with, restored};

fn open(cx: &mut gpui::TestAppContext, cursor: Option<&str>) -> (Harness, gpui::VisualTestContext) {
    open_with(
        cx,
        Some(builtin_config()),
        Some(restored(Some("SPX"), None, cursor)),
    )
}

impl Harness {
    pub(super) fn cursor(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile
            .read_with(vcx, |t, _| t.cursor().map(str::to_string))
    }

    /// The stages the model paints as the cursor's card.
    fn cursor_cards(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.tile.read_with(vcx, |t, _| {
            t.model()
                .map(|m| {
                    m.cards
                        .iter()
                        .filter(|c| c.cursor)
                        .map(|c| c.stage.clone())
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    /// The stages the model paints lit (not dimmed), in layout order.
    fn lit_cards(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.tile.read_with(vcx, |t, _| {
            t.model()
                .map(|m| {
                    m.cards
                        .iter()
                        .filter(|c| !c.dimmed)
                        .map(|c| c.stage.clone())
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    fn mode(&self, vcx: &gpui::VisualTestContext) -> KeyContext {
        self.key_context(vcx)
    }

    fn filter(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile
            .read_with(vcx, |t, _| t.filter().map(str::to_string))
    }

    fn scroll(&self, vcx: &gpui::VisualTestContext) -> (f32, f32) {
        self.tile.read_with(vcx, |t, _| t.scroll())
    }

    /// `(viewport width, content width)` as the model was last built.
    fn widths(&self, vcx: &gpui::VisualTestContext) -> (f32, f32) {
        self.tile.read_with(vcx, |t, _| {
            let content = t.model().expect("a pipeline is shown").layout.content_w;
            (t.viewport().0, content)
        })
    }
}

fn normal() -> KeyContext {
    KeyContext::new(crate::KIND).pair("mode", "normal")
}

fn insert() -> KeyContext {
    KeyContext::new(crate::KIND).pair("mode", "insert")
}

/// `l` follows the first consumer in document order, `h` the first input,
/// `j` and `k` walk the layer without wrapping. From `parity`: its first
/// consumer is `implied_rates` (the document names it before
/// `df_monotone`), whose first consumer is `repo_fit`; `box_fit` is the
/// next row of that layer; `implied_rates` is `repo_fit`'s first input.
#[gpui::test]
fn hjkl_move_the_cursor_along_inputs_consumers_and_layers(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx, Some("parity"));
    assert_eq!(h.cursor(&vcx).as_deref(), Some("parity"));
    assert_eq!(h.cursor_cards(&vcx), ["parity"]);

    h.press(&mut vcx, "l");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("implied_rates"));
    assert_eq!(h.cursor_cards(&vcx), ["implied_rates"], "the model follows");
    h.press(&mut vcx, "l");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("repo_fit"));
    h.press(&mut vcx, "j");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("box_fit"));
    h.press(&mut vcx, "j");
    assert_eq!(
        h.cursor(&vcx).as_deref(),
        Some("box_fit"),
        "the bottom of a layer does not wrap"
    );
    h.press(&mut vcx, "h");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("implied_rates"));
    h.press(&mut vcx, "k");
    assert_eq!(
        h.cursor(&vcx).as_deref(),
        Some("df_monotone"),
        "layer 2 reads [df_monotone, implied_rates]"
    );
    h.press(&mut vcx, "k");
    assert_eq!(
        h.cursor(&vcx).as_deref(),
        Some("df_monotone"),
        "the top of a layer does not wrap"
    );
    assert_eq!(
        h.dispatched(),
        [
            "calibration::cursor_right",
            "calibration::cursor_right",
            "calibration::cursor_down",
            "calibration::cursor_down",
            "calibration::cursor_left",
            "calibration::cursor_up",
            "calibration::cursor_up",
        ]
    );
    // The shared motion ids reach the same moves, as the palette sends them.
    assert!(h.act(&mut vcx, geode_tile::motion::DOWN));
    assert_eq!(h.cursor(&vcx).as_deref(), Some("implied_rates"));
    assert!(h.act(&mut vcx, geode_tile::motion::LEFT));
    assert_eq!(h.cursor(&vcx).as_deref(), Some("parity"));
}

/// `g N` lands on the Nth card in layout order (layer-major, barycentre
/// rows): 1 chain, 2 dividends, 3 rates, 4 parity, 5 df_monotone,
/// 6 implied_rates, 7 repo_fit, 8 box_fit, 9 repo_curve.
#[gpui::test]
fn g_digit_jumps_to_the_nth_stage_in_layout_order(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx, Some("parity"));
    h.press(&mut vcx, "g 5");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("df_monotone"));
    h.press(&mut vcx, "g 7");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("repo_fit"));
    h.press(&mut vcx, "g 1");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("chain"));
    h.press(&mut vcx, "g 9");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("repo_curve"));
    assert_eq!(
        h.dispatched(),
        [
            "calibration::jump_5",
            "calibration::jump_7",
            "calibration::jump_1",
            "calibration::jump_9",
        ]
    );
    // A bare digit is no jump: nothing is bound to it here.
    h.press(&mut vcx, "4");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("repo_curve"));
    assert_eq!(h.dispatched().len(), 4);
}

/// `/` opens the filter in the footer and the tile holds the keys in
/// `insert`; typing dims the cards that fail the fuzzy match as it goes;
/// `escape` restores the filter in force when `/` was pressed; `enter`
/// keeps the typed filter and lands the cursor on the first lit card in
/// layout order.
#[gpui::test]
fn slash_filters_cards_esc_reverts_and_enter_lands_on_the_first_match(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx, Some("parity"));
    assert_eq!(h.mode(&vcx), normal());
    assert_eq!(h.lit_cards(&vcx).len(), 11, "no filter: every card lit");

    h.press(&mut vcx, "/");
    assert_eq!(h.dispatched(), ["calibration::filter"]);
    assert_eq!(h.mode(&vcx), insert());
    h.draw(&mut vcx);
    assert!(
        vcx.debug_bounds("calibration-filter-7").is_some(),
        "the field is painted in the footer"
    );
    vcx.simulate_input("zzz");
    vcx.run_until_parked();
    assert_eq!(h.filter(&vcx).as_deref(), Some("zzz"), "typed as it goes");
    assert!(h.lit_cards(&vcx).is_empty(), "nothing matches zzz");
    // A bare `j` is text for the field, not a cursor step.
    vcx.simulate_keystrokes("j");
    vcx.run_until_parked();
    assert_eq!(h.filter(&vcx).as_deref(), Some("zzzj"));
    assert_eq!(h.cursor(&vcx).as_deref(), Some("parity"));
    assert_eq!(h.dispatched().len(), 1, "j reached no binding");
    h.press(&mut vcx, "escape");
    assert_eq!(
        h.dispatched().last().map(String::as_str),
        Some("calibration::cancel")
    );
    assert_eq!(h.mode(&vcx), normal());
    assert_eq!(h.filter(&vcx), None, "reverted to the entry query: none");
    assert_eq!(h.lit_cards(&vcx).len(), 11);
    assert_eq!(h.cursor(&vcx).as_deref(), Some("parity"), "unchanged");

    h.press(&mut vcx, "/");
    vcx.simulate_input("curve");
    vcx.run_until_parked();
    assert_eq!(h.lit_cards(&vcx), ["repo_curve", "box_curve"]);
    h.press(&mut vcx, "enter");
    assert_eq!(
        h.dispatched().last().map(String::as_str),
        Some("calibration::commit")
    );
    assert_eq!(h.mode(&vcx), normal());
    assert_eq!(
        h.cursor(&vcx).as_deref(),
        Some("repo_curve"),
        "the first lit card"
    );
    assert_eq!(h.filter(&vcx).as_deref(), Some("curve"), "kept");
    assert_eq!(
        h.lit_cards(&vcx),
        ["repo_curve", "box_curve"],
        "still dimmed"
    );

    // The kept filter is the next `/`'s entry query: `escape` restores it.
    h.press(&mut vcx, "/");
    vcx.simulate_input("x");
    vcx.run_until_parked();
    assert_eq!(h.filter(&vcx).as_deref(), Some("curvex"));
    h.press(&mut vcx, "escape");
    assert_eq!(h.filter(&vcx).as_deref(), Some("curve"));
    // And an emptied field committed clears it.
    h.press(&mut vcx, "/");
    vcx.simulate_keystrokes("backspace backspace backspace backspace backspace");
    vcx.run_until_parked();
    h.press(&mut vcx, "enter");
    assert_eq!(h.filter(&vcx), None);
    assert_eq!(h.lit_cards(&vcx).len(), 11);
    assert_eq!(h.mode(&vcx), normal());
}

/// The cursor moves write the session key, so a restored tile lands where
/// the trader left it; with no cursor the first move lands on the first
/// card in layout order.
#[gpui::test]
fn the_cursor_is_remembered_in_session_state(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx, None);
    assert_eq!(h.cursor(&vcx), None);
    assert!(h.serialize(&vcx).get("cursor").is_none());
    h.press(&mut vcx, "l");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("chain"));
    h.press(&mut vcx, "g 4");
    assert_eq!(
        h.serialize(&vcx),
        restored(Some("SPX"), Some("implied_forwards"), Some("parity"))
    );
}

/// At 400 px the six columns need more than the tile gives (the gaps hold
/// their floor), so the content runs past the viewport; a cursor move to
/// the last column scrolls the graph so the cursor's card is in view.
#[gpui::test]
fn a_narrow_tile_overflows_and_the_cursor_scrolls_into_view(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx, Some("parity"));
    vcx.simulate_resize(size(px(400.0), px(300.0)));
    h.draw(&mut vcx);
    let (viewport_w, content_w) = h.widths(&vcx);
    assert!(
        viewport_w > 0.0 && viewport_w <= 400.0,
        "the body was measured: {viewport_w}"
    );
    assert!(
        content_w > viewport_w,
        "six columns overflow {viewport_w}: {content_w}"
    );
    assert_eq!(h.scroll(&vcx), (0.0, 0.0), "parity is in view");

    // parity → implied_rates → repo_fit → repo_curve → publish.
    h.press(&mut vcx, "l l l l");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("publish"));
    let (sx, _) = h.scroll(&vcx);
    assert!(sx > 0.0, "scrolled right: {sx}");
    h.draw(&mut vcx);
    let body = vcx
        .debug_bounds("calibration-graph-7")
        .expect("the body paints");
    let card = vcx
        .debug_bounds("calibration-card-7-publish")
        .expect("the cursor's card paints");
    assert!(
        card.left() >= body.left() && card.right() <= body.right() + px(0.5),
        "the card is within the body: {card:?} in {body:?}"
    );
    // Back at the first column the graph scrolls home.
    h.press(&mut vcx, "g 1");
    assert_eq!(h.scroll(&vcx).0, 0.0);
    // `:layout` resets the scroll and relays the graph.
    h.press(&mut vcx, "g 9");
    assert!(h.scroll(&vcx).0 > 0.0);
    h.command(&mut vcx, "layout").unwrap();
    assert_eq!(h.scroll(&vcx), (0.0, 0.0));
}
