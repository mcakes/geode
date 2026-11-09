//! The pointer routes, each the twin of a key: a press on a card is the
//! cursor, a double press `enter`, a right press `.` at the pointer, the
//! wheel the tile's own scroll, a port dragged onto a card `c` + `enter`
//! through the same connect, a press on a header notice the cursor on the
//! stage it names. Every event is a real mouse event on the painted
//! elements.

use geode_core::calibration::CALIBRATION_DOC;
use geode_shell::actions::ActionId;
use geode_shell::keymap::KeyContext;
use geode_shell::tiling::TileId;
use gpui::{
    Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    ScrollDelta, ScrollWheelEvent, TouchPhase, VisualTestContext, point, px, size,
};

use super::edits::{inputs_of, open_publish_config};
use super::harness::{Harness, builtin_config, dropped_config, open_with, restored, warned_config};
use crate::core::menu::OPEN_STAGE;
use crate::core::model::EdgeTone;

fn open_spx(
    cx: &mut gpui::TestAppContext,
    config: crate::content::CalibrationConfig,
    cursor: Option<&str>,
) -> (Harness, VisualTestContext) {
    let (h, mut vcx) = open_with(cx, Some(config), Some(restored(Some("SPX"), None, cursor)));
    h.draw(&mut vcx);
    (h, vcx)
}

fn centre_of(vcx: &mut VisualTestContext, selector: &str) -> Point<Pixels> {
    let selector: &'static str = Box::leak(selector.to_string().into_boxed_str());
    vcx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is painted"))
        .center()
}

/// A point just inside `selector`'s left edge: a header notice is cut at
/// the cluster's share of the header, so its centre may lie past the clip.
fn left_of(vcx: &mut VisualTestContext, selector: &str) -> Point<Pixels> {
    let selector: &'static str = Box::leak(selector.to_string().into_boxed_str());
    let b = vcx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is painted"));
    point(b.left() + px(4.), b.center().y)
}

fn down(vcx: &mut VisualTestContext, at: Point<Pixels>, button: MouseButton, click_count: usize) {
    vcx.simulate_event(MouseDownEvent {
        position: at,
        modifiers: Modifiers::default(),
        button,
        click_count,
        first_mouse: false,
    });
}

fn up(vcx: &mut VisualTestContext, at: Point<Pixels>, button: MouseButton) {
    vcx.simulate_event(MouseUpEvent {
        position: at,
        modifiers: Modifiers::default(),
        button,
        click_count: 1,
    });
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

fn click(vcx: &mut VisualTestContext, at: Point<Pixels>, click_count: usize) {
    down(vcx, at, MouseButton::Left, click_count);
    up(vcx, at, MouseButton::Left);
}

fn right_click(vcx: &mut VisualTestContext, at: Point<Pixels>) {
    down(vcx, at, MouseButton::Right, 1);
    up(vcx, at, MouseButton::Right);
}

fn held_move(vcx: &mut VisualTestContext, at: Point<Pixels>) {
    vcx.simulate_event(MouseMoveEvent {
        position: at,
        pressed_button: Some(MouseButton::Left),
        modifiers: Modifiers::default(),
    });
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

/// Press the port of `from`'s card and drag it (past gpui's threshold)
/// over `to`'s card, the button still held.
fn drag_port_over(vcx: &mut VisualTestContext, from: &str, to: &str) -> Point<Pixels> {
    let port = centre_of(vcx, &format!("calibration-port-{}-{from}", Harness::TILE));
    let target = centre_of(vcx, &format!("calibration-card-{}-{to}", Harness::TILE));
    down(vcx, port, MouseButton::Left, 1);
    held_move(vcx, point(port.x, port.y + px(6.)));
    held_move(vcx, target);
    target
}

fn wheel(vcx: &mut VisualTestContext, at: Point<Pixels>, dx: f32, dy: f32, shift: bool) {
    vcx.simulate_event(ScrollWheelEvent {
        position: at,
        delta: ScrollDelta::Pixels(point(px(dx), px(dy))),
        modifiers: Modifiers {
            shift,
            ..Modifiers::default()
        },
        touch_phase: TouchPhase::Moved,
    });
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

fn actions_menu() -> KeyContext {
    KeyContext::new(crate::KIND)
        .pair("mode", "menu")
        .pair("popup", "actions")
        .tilelist()
}

impl Harness {
    fn painted_preview(&self, vcx: &VisualTestContext) -> Option<(String, String)> {
        self.tile.read_with(vcx, |t, _| {
            t.model()?
                .edges
                .iter()
                .find(|e| e.tone == EdgeTone::Preview)
                .map(|e| (e.from.clone(), e.to.clone()))
        })
    }

    fn offset(&self, vcx: &VisualTestContext) -> (f32, f32) {
        self.tile.read_with(vcx, |t, _| t.scroll())
    }

    /// The most the graph scrolls on each axis: `content - viewport`,
    /// never negative.
    fn max_scroll(&self, vcx: &VisualTestContext) -> (f32, f32) {
        self.tile.read_with(vcx, |t, _| {
            let (w, h, _) = t.viewport();
            let layout = &t.model().expect("a graph").layout;
            (
                (layout.content_w - w).max(0.0),
                (layout.content_h - h).max(0.0),
            )
        })
    }

    fn menu_is_up(&self, vcx: &VisualTestContext) -> bool {
        self.tile.read_with(vcx, |t, _| t.menu_open().is_some())
    }
}

/// A left press on a card is the cursor, as `h j k l` are; a second press
/// (gpui's `click_count` 2) is `enter`: the shell is asked for
/// `config::calibration` with the card's stage as the object. A modified
/// press is the shell's (its tile drag and fullscreen) and moves nothing.
#[gpui::test]
fn a_click_moves_the_cursor_and_a_double_click_opens_the_stage(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx, builtin_config(), Some("parity"));
    let repo_fit = centre_of(&mut vcx, "calibration-card-7-repo_fit");
    click(&mut vcx, repo_fit, 1);
    assert_eq!(h.cursor(&vcx).as_deref(), Some("repo_fit"));
    assert!(
        h.requested_actions(&mut vcx).is_empty(),
        "a click opens nothing"
    );
    assert_eq!(h.dispatched(), Vec::<String>::new(), "no key was involved");

    let box_fit = centre_of(&mut vcx, "calibration-card-7-box_fit");
    click(&mut vcx, box_fit, 2);
    assert_eq!(h.cursor(&vcx).as_deref(), Some("box_fit"));
    assert_eq!(
        h.requested_actions(&mut vcx),
        vec![(TileId(Harness::TILE), ActionId(OPEN_STAGE.to_string()))],
        "the enter route"
    );
    assert_eq!(
        h.config_object(&vcx)
            .map(|o| (o.doc, o.name, o.seed.is_none())),
        Some((CALIBRATION_DOC, "box_fit".to_string(), true))
    );

    // alt+press is the shell's: the cursor stays.
    let chain = centre_of(&mut vcx, "calibration-card-7-chain");
    vcx.simulate_event(MouseDownEvent {
        position: chain,
        modifiers: Modifiers {
            alt: true,
            ..Modifiers::default()
        },
        button: MouseButton::Left,
        click_count: 1,
        first_mouse: false,
    });
    vcx.simulate_event(MouseUpEvent {
        position: chain,
        modifiers: Modifiers {
            alt: true,
            ..Modifiers::default()
        },
        button: MouseButton::Left,
        click_count: 1,
    });
    assert_eq!(
        h.cursor(&vcx).as_deref(),
        Some("box_fit"),
        "the shell's press"
    );
}

/// A right press on a card puts the cursor on it and opens the `.` menu
/// hung from the pointer, not the header; `escape` closes it as it closes
/// the keyboard's. A right press on the empty body opens the menu for the
/// cursor as it stands.
#[gpui::test]
fn a_right_click_opens_the_menu_at_the_card(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx, builtin_config(), Some("chain"));
    let at = centre_of(&mut vcx, "calibration-card-7-parity");
    right_click(&mut vcx, at);
    assert_eq!(h.cursor(&vcx).as_deref(), Some("parity"));
    assert!(h.menu_is_up(&vcx));
    assert_eq!(h.key_context(&vcx), actions_menu());
    let menu = vcx
        .debug_bounds("calibration-menu-7")
        .expect("the menu paints");
    assert!(
        (menu.left() - at.x).abs() <= px(2.) && (menu.top() - at.y).abs() <= px(2.),
        "the menu hangs from the pointer: {menu:?} at {at:?}"
    );
    let header = vcx.debug_bounds("calibration-header-7").unwrap();
    assert!(menu.top() > header.bottom(), "not the header's menu");

    h.press(&mut vcx, "escape");
    assert!(!h.menu_is_up(&vcx));
    assert_eq!(h.dispatched(), ["calibration::cancel"]);

    // The empty body: the cursor stays, the menu opens where pressed.
    let body = vcx.debug_bounds("calibration-graph-7").unwrap();
    let empty = point(body.left() + px(4.), body.top() + px(4.));
    right_click(&mut vcx, empty);
    assert_eq!(h.cursor(&vcx).as_deref(), Some("parity"), "unmoved");
    assert!(h.menu_is_up(&vcx));
    let menu = vcx.debug_bounds("calibration-menu-7").unwrap();
    assert!(
        (menu.left() - empty.x).abs() <= px(2.) && (menu.top() - empty.y).abs() <= px(2.),
        "{menu:?} at {empty:?}"
    );
    // `.` closes it, as it closes the keyboard's.
    h.press(&mut vcx, ".");
    assert!(!h.menu_is_up(&vcx));
}

/// At 400 px the graph overflows; the wheel scrolls it by its pixel delta
/// and stops at the content's edge on both ends. A horizontal delta or
/// shift with a vertical one scrolls x.
#[gpui::test]
fn the_wheel_scrolls_an_overflowing_graph_and_clamps(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx, builtin_config(), Some("chain"));
    vcx.simulate_resize(size(px(400.0), px(300.0)));
    h.draw(&mut vcx);
    let (max_x, max_y) = h.max_scroll(&vcx);
    assert!(max_x > 0.0, "the columns overflow: {max_x}");
    assert!(max_y > 0.0, "the rows overflow: {max_y}");
    assert_eq!(h.offset(&vcx), (0.0, 0.0));
    let body = vcx.debug_bounds("calibration-graph-7").unwrap();
    let at = body.center();

    wheel(&mut vcx, at, -40.0, 0.0, false);
    assert_eq!(
        h.offset(&vcx),
        (40.0_f32.min(max_x), 0.0),
        "right by the delta"
    );
    wheel(&mut vcx, at, -10_000.0, 0.0, false);
    assert_eq!(h.offset(&vcx).0, max_x, "clamped at the content's edge");
    wheel(&mut vcx, at, 10_000.0, 0.0, false);
    assert_eq!(h.offset(&vcx).0, 0.0, "clamped at home");

    // shift+wheel: the vertical delta scrolls x.
    wheel(&mut vcx, at, 0.0, -30.0, true);
    assert_eq!(h.offset(&vcx), (30.0_f32.min(max_x), 0.0));

    // y clamps the same way, whatever the content's height.
    wheel(&mut vcx, at, 0.0, -10_000.0, false);
    assert_eq!(h.offset(&vcx).1, max_y);
    wheel(&mut vcx, at, 0.0, 10_000.0, false);
    assert_eq!(h.offset(&vcx).1, 0.0);
    h.draw(&mut vcx);
    assert!(
        vcx.debug_bounds("calibration-card-7-chain").is_some(),
        "the graph still paints"
    );
}

/// The publish stage reads one raster and no gate: `box_curve` may feed
/// it. Its port dragged over `publish` previews the edge as the connect
/// picker does; the drop queues the same whole-object edit `c` + `enter`
/// would, and the preview leaves with the drag.
#[gpui::test]
fn dragging_a_port_onto_an_eligible_card_queues_the_connect_edit(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx, open_publish_config(), Some("parity"));
    assert!(
        vcx.debug_bounds("calibration-port-7-box_curve").is_some(),
        "each card has a port"
    );
    let target = drag_port_over(&mut vcx, "box_curve", "publish");
    assert!(vcx.update(|_, cx| cx.has_active_drag()), "a drag is live");
    assert_eq!(
        h.painted_preview(&vcx),
        Some(("box_curve".to_string(), "publish".to_string())),
        "the edge previews over an eligible card"
    );
    assert!(h.config_edits(&mut vcx).is_empty(), "nothing queued yet");

    up(&mut vcx, target, MouseButton::Left);
    assert!(!vcx.update(|_, cx| cx.has_active_drag()));
    assert_eq!(
        h.painted_preview(&vcx),
        None,
        "the preview leaves with the drag"
    );
    let edits = h.config_edits(&mut vcx);
    assert_eq!(edits.len(), 1, "{edits:?}");
    assert_eq!(edits[0].doc, CALIBRATION_DOC);
    assert_eq!(edits[0].object, "publish");
    assert_eq!(edits[0].origin, Some(TileId(Harness::TILE)));
    assert_eq!(inputs_of(&edits[0]), ["repo_curve", "box_curve"]);
    let table = edits[0].value.as_ref().unwrap().as_table().unwrap();
    assert_eq!(table["kind"].as_str(), Some("publish"));
    assert_eq!(table["target"].as_str(), Some("sophis"));
    assert!(h.notices(&vcx).is_empty());
    assert_eq!(
        h.cursor(&vcx).as_deref(),
        Some("publish"),
        "the cursor lands on the consumer, where c + enter leaves it"
    );
}

/// `escape` during a live drag ends it through the keymap's `cancel`, as
/// it dismisses any topmost surface: no drag, no preview, and the release
/// that follows queues nothing. The press is spent on the drag alone.
#[gpui::test]
fn escape_ends_a_live_port_drag(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx, open_publish_config(), Some("parity"));
    let target = drag_port_over(&mut vcx, "box_curve", "publish");
    assert!(vcx.update(|_, cx| cx.has_active_drag()));
    assert!(h.painted_preview(&vcx).is_some());
    h.press(&mut vcx, "escape");
    assert_eq!(h.dispatched(), ["calibration::cancel"]);
    assert!(!vcx.update(|_, cx| cx.has_active_drag()), "the drag ended");
    assert_eq!(h.painted_preview(&vcx), None, "the preview left with it");
    up(&mut vcx, target, MouseButton::Left);
    assert!(h.config_edits(&mut vcx).is_empty(), "nothing queued");
    assert!(h.notices(&vcx).is_empty());
    assert_eq!(
        h.cursor(&vcx).as_deref(),
        Some("box_curve"),
        "the pressed card"
    );
}

/// A source emits documents, which no publish port takes, and `repo_fit`'s
/// one port is filled: neither card may take the drop. Over them nothing
/// previews, the release queues nothing and says nothing, since the drop
/// was never offered.
#[gpui::test]
fn an_ineligible_card_refuses_the_drop(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx, builtin_config(), Some("parity"));
    let target = drag_port_over(&mut vcx, "chain", "publish");
    assert!(vcx.update(|_, cx| cx.has_active_drag()));
    assert_eq!(
        h.painted_preview(&vcx),
        None,
        "no preview over an ineligible card"
    );
    up(&mut vcx, target, MouseButton::Left);
    assert!(!vcx.update(|_, cx| cx.has_active_drag()), "the drag ended");
    assert!(h.config_edits(&mut vcx).is_empty(), "nothing queued");
    assert!(
        h.notices(&vcx).is_empty(),
        "nothing refused: the drop was never offered"
    );

    let target = drag_port_over(&mut vcx, "box_curve", "repo_fit");
    assert_eq!(h.painted_preview(&vcx), None);
    up(&mut vcx, target, MouseButton::Left);
    assert!(h.config_edits(&mut vcx).is_empty());
    assert!(h.notices(&vcx).is_empty());
    assert_eq!(
        h.cursor(&vcx).as_deref(),
        Some("box_curve"),
        "the pressed card, not the refused target"
    );
}

/// A finding at `stages.repo_fit.knots` is a header notice; a press on it
/// lands the cursor on `repo_fit` (the held graph still has the card) and
/// dismisses it, as a press always has. A finding naming a stage the
/// shown graph lacks only dismisses.
#[gpui::test]
fn clicking_the_header_notice_moves_the_cursor_to_the_offending_stage(
    cx: &mut gpui::TestAppContext,
) {
    // A valid graph first, then the reload that drops it: the graph is
    // held under the finding, so `repo_fit` still has a card.
    let (h, mut vcx) = open_spx(cx, builtin_config(), Some("parity"));
    h.set_config(&mut vcx, dropped_config());
    h.draw(&mut vcx);
    let notices = h.notices(&vcx);
    assert_eq!(
        notices.first().map(String::as_str),
        Some("calibration: stage 'repo_fit': knots is two or more strictly ascending numbers")
    );
    let at = left_of(&mut vcx, "tile-notice-7-0");
    click(&mut vcx, at, 1);
    assert_eq!(
        h.cursor(&vcx).as_deref(),
        Some("repo_fit"),
        "the offending stage"
    );
    assert!(
        !h.notices(&vcx).contains(&notices[0]),
        "dismissed as well: {:?}",
        h.notices(&vcx)
    );

    let (h, mut vcx) = open_spx(cx, warned_config(), Some("parity"));
    assert_eq!(h.notices(&vcx).len(), 1);
    let at = left_of(&mut vcx, "tile-notice-7-0");
    click(&mut vcx, at, 1);
    assert_eq!(
        h.cursor(&vcx).as_deref(),
        Some("parity"),
        "src is no card here"
    );
    assert!(h.notices(&vcx).is_empty(), "dismissed");
}
