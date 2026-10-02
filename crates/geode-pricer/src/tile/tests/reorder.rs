//! Row reorder by the grip, through real mouse events: the grip shows on
//! a hovered movable row, a drag of it paints the drop line at the
//! nearest legal gap and lands one undo entry on release; an illegal
//! drop, a release elsewhere and `escape` move nothing; the grip's own
//! press never selects, moves the cursor or edits.

use super::*;
use gpui::{
    Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, px,
};

const FOUR: [&str; 4] = [
    "SPX Z26 1000 P",
    "SPX Z26 2000 P",
    "SPX Z26 3000 P",
    "SPX Z26 4000 P",
];

fn cursor_text(h: &Harness, vcx: &VisualTestContext) -> Option<String> {
    let row = h.cursor(vcx)?.0;
    h.tree(vcx).get(row).cloned()
}

fn row_of(h: &Harness, vcx: &VisualTestContext, text: &str) -> usize {
    h.tree(vcx)
        .iter()
        .position(|t| t == text)
        .unwrap_or_else(|| panic!("no row '{text}' in {:?}", h.tree(vcx)))
}

fn hover(vcx: &mut VisualTestContext, at: Point<Pixels>) {
    vcx.simulate_mouse_move(at, None, Modifiers::default());
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

/// The point over grid row `row`'s first measure cell: its upper half,
/// or its lower half when `lower`.
fn over_row(vcx: &mut VisualTestContext, row: usize, lower: bool) -> Point<Pixels> {
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let selector: &'static str = Box::leak(format!("pricer-cell-{row}-1").into_boxed_str());
    let b = vcx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is painted"));
    let y = if lower {
        b.top() + b.size.height * 0.75
    } else {
        b.top() + b.size.height * 0.25
    };
    gpui::point(b.center().x, y)
}

/// Hover grid row `row` until its grip shows, and answer the grip's
/// centre.
fn grip(vcx: &mut VisualTestContext, row: usize) -> Point<Pixels> {
    let on_row = over_row(vcx, row, false);
    hover(vcx, on_row);
    centre_of(vcx, &format!("pricer-grip-{row}"))
}

fn down(vcx: &mut VisualTestContext, at: Point<Pixels>, click_count: usize) {
    vcx.simulate_event(MouseDownEvent {
        position: at,
        modifiers: Modifiers::default(),
        button: MouseButton::Left,
        click_count,
        first_mouse: false,
    });
}

fn up(vcx: &mut VisualTestContext, at: Point<Pixels>, click_count: usize) {
    vcx.simulate_event(MouseUpEvent {
        position: at,
        modifiers: Modifiers::default(),
        button: MouseButton::Left,
        click_count,
    });
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
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

/// Press row `row`'s grip and drag it (past gpui's threshold) to `to`,
/// the button still held.
fn start_drag(vcx: &mut VisualTestContext, row: usize, to: Point<Pixels>) {
    let at = grip(vcx, row);
    down(vcx, at, 1);
    held_move(vcx, gpui::point(at.x, at.y + px(6.)));
    held_move(vcx, to);
}

/// Drag row `row`'s grip to `to` and release there.
fn drag_to(vcx: &mut VisualTestContext, row: usize, to: Point<Pixels>) {
    start_drag(vcx, row, to);
    up(vcx, to, 1);
}

fn drop_gap(h: &Harness, vcx: &VisualTestContext) -> Option<usize> {
    h.tile
        .read_with(vcx, |t, cx| t.table.read(cx).delegate().drop_gap)
}

fn selected(h: &Harness, vcx: &VisualTestContext) -> bool {
    h.tile.read_with(vcx, |t, _| t.selection.is_some())
}

/// A row dragged down lands below the row the pointer's lower half is
/// over, as one `Edit::Move`: the cursor lands on it, the footer stays
/// clear, one undo restores. Up, the pointer's upper half lands it above.
#[gpui::test]
fn a_grip_drag_moves_a_row_down_and_up(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &FOUR);
    let to = over_row(&mut vcx, 2, true);
    start_drag(&mut vcx, 0, to);
    assert_eq!(drop_gap(&h, &vcx), Some(3), "the line below row 2");
    assert!(
        vcx.debug_bounds("pricer-drop-line").is_some(),
        "the drop line paints"
    );
    up(&mut vcx, to, 1);
    assert_eq!(
        h.tree(&vcx),
        [
            "SPX Z26 2000 P",
            "SPX Z26 3000 P",
            "SPX Z26 1000 P",
            "SPX Z26 4000 P"
        ]
    );
    assert_eq!(cursor_text(&h, &vcx).as_deref(), Some("SPX Z26 1000 P"));
    assert_eq!(h.footer(&vcx), None);
    assert_eq!(drop_gap(&h, &vcx), None);
    assert!(vcx.debug_bounds("pricer-drop-line").is_none());
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(h.tree(&vcx), FOUR, "one undo step");
    let to = over_row(&mut vcx, 1, false);
    drag_to(&mut vcx, 3, to);
    assert_eq!(
        h.tree(&vcx),
        [
            "SPX Z26 1000 P",
            "SPX Z26 4000 P",
            "SPX Z26 2000 P",
            "SPX Z26 3000 P"
        ]
    );
    assert_eq!(cursor_text(&h, &vcx).as_deref(), Some("SPX Z26 4000 P"));
}

/// A grip on a row inside a live `V` selection drags the whole selection,
/// keeps it live, and lands as ONE undo entry.
#[gpui::test]
fn a_grip_drag_moves_a_v_block_as_one_undo(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &FOUR);
    h.motion(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.motion(&mut vcx, "down", None);
    let to = over_row(&mut vcx, 3, true);
    drag_to(&mut vcx, 1, to);
    assert_eq!(
        h.tree(&vcx),
        [
            "SPX Z26 1000 P",
            "SPX Z26 4000 P",
            "SPX Z26 2000 P",
            "SPX Z26 3000 P"
        ]
    );
    assert!(selected(&h, &vcx), "the selection rides along");
    assert_eq!(
        cursor_text(&h, &vcx).as_deref(),
        Some("SPX Z26 3000 P"),
        "the cursor keeps the selection's end"
    );
    assert_eq!(h.footer(&vcx), None);
    h.dispatch(&mut vcx, "escape", None);
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(h.tree(&vcx), FOUR, "both moves undo as one");
}

/// A leg drags among its package's legs; the pointer outside the
/// package drops nowhere.
#[gpui::test]
fn a_grip_drag_moves_a_leg_within_its_package(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 1000 P", "-5 SPX Z26 4800/5200 CS"]);
    h.dispatch(&mut vcx, "expand_all", None);
    let legs: Vec<usize> = h.tile.read_with(&vcx, |t, _| {
        (0..t.model.len())
            .filter(|&g| matches!(t.model.kind(g), Some(GridRowKind::Leg { .. })))
            .collect()
    });
    let (first, second) = (h.tree(&vcx)[legs[0]].clone(), h.tree(&vcx)[legs[1]].clone());
    let outside = over_row(&mut vcx, 0, false);
    start_drag(&mut vcx, legs[0], outside);
    assert_eq!(drop_gap(&h, &vcx), None, "above the package");
    let to = over_row(&mut vcx, legs[1], true);
    held_move(&mut vcx, to);
    assert_eq!(drop_gap(&h, &vcx), Some(legs[1] + 1));
    up(&mut vcx, to, 1);
    assert_eq!(h.tree(&vcx)[legs[0]], second);
    assert_eq!(h.tree(&vcx)[legs[1]], first);
}

/// Under a value grouping a row drags within its own group; over the
/// other group's rows no line shows and the release moves nothing.
#[gpui::test]
fn a_grouped_grip_drag_stays_in_its_group(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(
        cx,
        &[
            "SPX Z26 4000 P",
            "NDX Z26 5000 C",
            "SPX Z26 4200 P",
            "NDX Z26 5200 C",
        ],
    );
    h.command(&mut vcx, "group underlying_ref").unwrap();
    h.dispatch(&mut vcx, "expand_all", None);
    let grouped = h.tree(&vcx);
    let spx = row_of(&h, &vcx, "SPX Z26 4000 P");
    let ndx = row_of(&h, &vcx, "NDX Z26 5000 C");
    let over_ndx = over_row(&mut vcx, ndx, true);
    drag_to(&mut vcx, spx, over_ndx);
    assert_eq!(h.tree(&vcx), grouped, "the other group is no drop");
    assert_eq!(drop_gap(&h, &vcx), None);
    let last = row_of(&h, &vcx, "SPX Z26 4200 P");
    let to = over_row(&mut vcx, last, true);
    drag_to(&mut vcx, spx, to);
    let tree = h.tree(&vcx);
    assert_eq!(tree[..3], grouped[..3], "NDX unchanged");
    assert_eq!(tree[3..], ["SPX", "SPX Z26 4200 P", "SPX Z26 4000 P"]);
    assert_eq!(h.footer(&vcx), None);
}

/// `escape` mid-drag ends it: the line goes, and the release that
/// follows moves nothing.
#[gpui::test]
fn escape_cancels_a_grip_drag(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &FOUR);
    let to = over_row(&mut vcx, 3, true);
    start_drag(&mut vcx, 0, to);
    assert_eq!(drop_gap(&h, &vcx), Some(4));
    key(&h, &mut vcx, "escape");
    assert_eq!(drop_gap(&h, &vcx), None);
    assert!(!vcx.update(|_, cx| cx.has_active_drag()), "the drag ended");
    up(&mut vcx, to, 1);
    assert_eq!(h.tree(&vcx), FOUR);
}

/// A real key: matched against the module's keymap fragment under the
/// tile's own key context, then dispatched through the shell's door.
fn key(h: &Harness, vcx: &mut VisualTestContext, key: &str) {
    use geode_shell::keymap::{MatchResult, Matcher, build_keymap, parse_keystroke};
    let mut registry = geode_shell::actions::ActionRegistry::default();
    geode_shell::defaults::register_builtin_actions(&mut registry);
    h.factory.register_actions(&mut registry);
    let doc =
        geode_shell::keymap::fragments::fragment_doc("pricer", crate::content::DEFAULT_KEYMAP)
            .unwrap();
    let (keymap, diags) = build_keymap(&[doc], geode_shell::defaults::default_mod(), &registry);
    assert!(diags.is_empty(), "{diags:?}");
    let stack = [h.tile.read_with(vcx, |t, _| t.key_context())];
    let ks = parse_keystroke(key, geode_shell::defaults::default_mod()).unwrap();
    let MatchResult::Matched { action, count } = Matcher::default().press(&keymap, ks, &stack)
    else {
        panic!("{key}: no binding");
    };
    assert!(vcx.update(|window, cx| h.content.dispatch(&action, count, window, cx)));
    h.draw(vcx);
}

const SIX: [&str; 6] = [
    "SPX Z26 1000 P",
    "SPX Z26 2000 P",
    "SPX Z26 3000 P",
    "SPX Z26 4000 P",
    "SPX Z26 5000 P",
    "SPX Z26 6000 P",
];

const ROW_MOVED: &str = "selection cleared: a row moved";

/// A grip drag of a row outside a live `V` selection ends the selection
/// as the drag starts: a selection kept over painted rows would widen to
/// take in the dragged line (and `d` would then delete it).
#[gpui::test]
fn dragging_a_row_outside_a_v_selection_ends_it(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &SIX);
    h.motion(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.motion(&mut vcx, "down", None);
    let to = over_row(&mut vcx, 1, true);
    start_drag(&mut vcx, 5, to);
    assert!(!selected(&h, &vcx), "ended at drag start");
    assert_eq!(h.footer(&vcx).as_deref(), Some(ROW_MOVED));
    assert_eq!(drop_gap(&h, &vcx), Some(2));
    up(&mut vcx, to, 1);
    assert_eq!(h.tree(&vcx)[2], "SPX Z26 6000 P");
    assert!(!selected(&h, &vcx));
}

/// Any grip drag under a `v` block ends it, the anchor's own line
/// included: a block is cells, not rows the drag could carry.
#[gpui::test]
fn dragging_a_row_under_a_v_block_ends_it(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &SIX);
    h.motion(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "visual_block", None);
    h.motion(&mut vcx, "down", Some(2));
    let to = over_row(&mut vcx, 5, true);
    start_drag(&mut vcx, 1, to);
    assert!(!selected(&h, &vcx));
    assert_eq!(h.footer(&vcx).as_deref(), Some(ROW_MOVED));
    up(&mut vcx, to, 1);
    assert_eq!(h.tree(&vcx)[5], "SPX Z26 2000 P");
    assert!(!selected(&h, &vcx));
}

/// A rebuild that makes moves refused mid-drag (a sort turned on) ends
/// the drop: no line shows and the release reorders nothing.
#[gpui::test]
fn a_sort_mid_drag_refuses_the_drop(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &FOUR);
    let to = over_row(&mut vcx, 3, true);
    start_drag(&mut vcx, 0, to);
    assert_eq!(drop_gap(&h, &vcx), Some(4));
    h.command(&mut vcx, "sort strike desc").unwrap();
    // The top of the sorted table: a gap that would move the dragged line
    // in sheet order, were the drop not refused.
    let top = over_row(&mut vcx, 0, false);
    held_move(&mut vcx, top);
    assert_eq!(drop_gap(&h, &vcx), None);
    up(&mut vcx, top, 1);
    let roots: Vec<String> = h.tile.read_with(&vcx, |t, _| {
        t.sheet.roots().map(|r| t.sheet.shorthand(r)).collect()
    });
    assert_eq!(roots, FOUR, "sheet order unchanged");
}

/// A refused drag never ends the selection: a package outside a live `V`
/// selection, its grip pressed, then partly hidden by the scope before the
/// drag moves — the drag says why, the selection stays, nothing moves.
#[gpui::test]
fn a_refused_drag_keeps_the_selection_and_its_footer(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(
        cx,
        &[
            "SPX Z26 1000 P",
            "SPX Z26 2000 P",
            "-5 SPX Z26 4800/5200 CS",
        ],
    );
    h.dispatch(&mut vcx, "expand_all", None);
    h.dispatch(&mut vcx, "visual_rows", None);
    let at = grip(&mut vcx, 2);
    down(&mut vcx, at, 1);
    // The grip overlaps the package's chevron slot: its press is still
    // the grip's, so the selection survives it.
    assert!(selected(&h, &vcx), "the press keeps the selection");
    let scope = geode_core::scope::Scope {
        expression: Some(geode_core::scope::parse_expr("strike != 5200").unwrap()),
        ..Default::default()
    };
    h.frame.update(&mut vcx, |f, cx| {
        f.shared_mut().set_scope(scope);
        cx.notify();
    });
    vcx.run_until_parked();
    held_move(&mut vcx, gpui::point(at.x, at.y + px(6.)));
    let top = over_row(&mut vcx, 0, false);
    held_move(&mut vcx, top);
    assert!(selected(&h, &vcx), "the selection stays");
    assert_eq!(
        h.footer(&vcx).as_deref(),
        Some("package partly hidden by the scope: edit its legs")
    );
    assert_eq!(drop_gap(&h, &vcx), None);
    up(&mut vcx, top, 1);
    assert_eq!(h.tree(&vcx)[0], "SPX Z26 1000 P", "nothing moved");
}

/// A grip click with no drag leaves no drag state behind.
#[gpui::test]
fn a_grip_click_leaves_no_drag(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &FOUR);
    let at = grip(&mut vcx, 1);
    down(&mut vcx, at, 1);
    assert!(h.tile.read_with(&vcx, |t, _| t.row_drag.is_some()));
    up(&mut vcx, at, 1);
    assert!(h.tile.read_with(&vcx, |t, _| t.row_drag.is_none()));
}

/// No line at a gap that changes nothing (the dragged row's own edges);
/// the empty body below the last row is the gap after it.
#[gpui::test]
fn no_line_at_a_no_op_gap_and_below_the_last_row_is_the_end(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &FOUR);
    let own = over_row(&mut vcx, 1, true);
    start_drag(&mut vcx, 1, own);
    assert_eq!(drop_gap(&h, &vcx), None, "below itself");
    let above = over_row(&mut vcx, 1, false);
    held_move(&mut vcx, above);
    assert_eq!(drop_gap(&h, &vcx), None, "above itself");
    let below = h.tile.read_with(&vcx, |t, cx| {
        let b = t
            .table
            .read(cx)
            .vertical_scroll_handle
            .0
            .borrow()
            .base_handle
            .bounds();
        gpui::point(b.center().x, b.bottom() - px(30.))
    });
    let last = over_row(&mut vcx, 3, true);
    assert!(
        below.y > last.y + px(26.),
        "fixture: empty body below the rows"
    );
    held_move(&mut vcx, below);
    assert_eq!(drop_gap(&h, &vcx), Some(4));
    up(&mut vcx, below, 1);
    assert_eq!(h.tree(&vcx)[3], "SPX Z26 2000 P");
}

/// A split package is a sibling in each group it paints under: dragging a
/// line above the calendar's row in the later expiry group lands it there,
/// beside that group's own half, and leaves the earlier group's order.
#[gpui::test]
fn a_grouped_drag_lands_beside_a_split_packages_own_half(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(
        cx,
        &["SPX Z26 4000 P", "SPX Z26/H27 5000 CAL", "SPX H27 4000 P"],
    );
    h.command(&mut vcx, "group expiry").unwrap();
    h.dispatch(&mut vcx, "expand_all", None);
    let splits: Vec<usize> = h.tile.read_with(&vcx, |t, _| {
        (0..t.model.len())
            .filter(|&g| {
                matches!(
                    t.model.kind(g),
                    Some(GridRowKind::Package { split: true, .. })
                )
            })
            .collect()
    });
    let line = row_of(&h, &vcx, "SPX H27 4000 P");
    assert!(splits[1] < line, "{:?}", h.tree(&vcx));
    let to = over_row(&mut vcx, splits[1], false);
    drag_to(&mut vcx, line, to);
    let roots: Vec<String> = h.tile.read_with(&vcx, |t, _| {
        t.sheet.roots().map(|r| t.sheet.shorthand(r)).collect()
    });
    assert_eq!(roots[1], "SPX H27 4000 P", "{roots:?}");
    assert_eq!(roots[0], "SPX Z26 4000 P");
}

/// A drag carried back and released on its own grip's cell is no click:
/// the cursor stays and nothing moves.
#[gpui::test]
fn a_drag_released_on_its_own_cell_does_not_click(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &FOUR);
    let cursor = h.cursor(&vcx);
    let at = grip(&mut vcx, 2);
    down(&mut vcx, at, 1);
    held_move(&mut vcx, gpui::point(at.x, at.y + px(6.)));
    let away = over_row(&mut vcx, 3, true);
    held_move(&mut vcx, away);
    held_move(&mut vcx, at);
    up(&mut vcx, at, 1);
    assert_eq!(h.tree(&vcx), FOUR);
    assert_eq!(h.cursor(&vcx), cursor, "the cursor stays");
}

/// A grip's press, click and double-click are the grip's alone: no
/// cursor move, no selection started or cleared, no editor.
#[gpui::test]
fn a_grip_press_never_selects_moves_the_cursor_or_edits(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &FOUR);
    let cursor = h.cursor(&vcx);
    let at = grip(&mut vcx, 2);
    down(&mut vcx, at, 1);
    up(&mut vcx, at, 1);
    down(&mut vcx, at, 2);
    up(&mut vcx, at, 2);
    assert_eq!(h.cursor(&vcx), cursor, "the cursor stays");
    assert!(!selected(&h, &vcx));
    assert!(
        h.tile.read_with(&vcx, |t, _| t.editor.is_none()),
        "no editor"
    );
    assert_eq!(h.mode(&mut vcx), "normal");
    // A live selection survives a grip click on a row outside it.
    h.dispatch(&mut vcx, "visual_rows", None);
    let at = grip(&mut vcx, 3);
    down(&mut vcx, at, 1);
    up(&mut vcx, at, 1);
    assert!(selected(&h, &vcx), "not cleared");
    assert_eq!(h.cursor(&vcx), cursor, "not extended");
    // An ordinary press elsewhere still clears it, as before.
    let cell = over_row(&mut vcx, 1, false);
    down(&mut vcx, cell, 1);
    up(&mut vcx, cell, 1);
    assert!(!selected(&h, &vcx));
}

/// No grip paints under a sort (sheet order is not the painted order) or
/// on a grouping row.
#[gpui::test]
fn no_grip_under_a_sort_or_on_a_group_row(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &FOUR);
    let _ = grip(&mut vcx, 0);
    h.command(&mut vcx, "sort strike desc").unwrap();
    let on_row = over_row(&mut vcx, 0, false);
    hover(&mut vcx, on_row);
    assert!(vcx.debug_bounds("pricer-grip-0").is_none(), "sorted");
    h.command(&mut vcx, "sort clear").unwrap();
    h.command(&mut vcx, "group underlying_ref").unwrap();
    h.dispatch(&mut vcx, "expand_all", None);
    assert!(h.tile.read_with(&vcx, |t, cx| {
        let d = t.table.read(cx).delegate();
        d.grips.first() == Some(&false) && d.grips[1..].iter().all(|&g| g)
    }));
}

fn grips(h: &Harness, vcx: &VisualTestContext) -> Vec<bool> {
    h.tile
        .read_with(vcx, |t, cx| t.table.read(cx).delegate().grips.clone())
}

/// A read-only row paints no grip: a package the grouping splits and its
/// legs (each refuses the keys), and a package the scope partly hides —
/// whose shown leg still moves.
#[gpui::test]
fn no_grip_on_split_or_partly_hidden_packages(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(
        cx,
        &["SPX Z26 4000 P", "SPX Z26/H27 5000 CAL", "SPX H27 4000 P"],
    );
    h.command(&mut vcx, "group expiry").unwrap();
    h.dispatch(&mut vcx, "expand_all", None);
    let kinds: Vec<Option<GridRowKind>> = h.tile.read_with(&vcx, |t, _| {
        (0..t.model.len()).map(|g| t.model.kind(g)).collect()
    });
    for (g, (kind, grip)) in kinds.iter().zip(grips(&h, &vcx)).enumerate() {
        let want = matches!(kind, Some(GridRowKind::Line));
        assert_eq!(grip, want, "row {g}: {kind:?}");
    }

    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 1000 P", "-5 SPX Z26 4800/5200 CS"]);
    h.dispatch(&mut vcx, "expand_all", None);
    let scope = geode_core::scope::Scope {
        expression: Some(geode_core::scope::parse_expr("strike != 5200").unwrap()),
        ..Default::default()
    };
    h.frame.update(&mut vcx, |f, cx| {
        f.shared_mut().set_scope(scope);
        cx.notify();
    });
    vcx.run_until_parked();
    assert_eq!(grips(&h, &vcx), [true, false, true], "{:?}", h.tree(&vcx));
}

/// Held at the body's bottom edge, the drag scrolls the table on its own
/// and keeps reading the gap under the still pointer.
#[gpui::test]
fn a_grip_drag_at_the_bottom_edge_scrolls(cx: &mut gpui::TestAppContext) {
    let lines: Vec<String> = (1..=80).map(|k| format!("SPX Z26 {} P", k * 10)).collect();
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let (h, mut vcx) = open_seeded(cx, &lines);
    let offset = |h: &Harness, vcx: &VisualTestContext| {
        h.tile.read_with(vcx, |t, cx| {
            t.table
                .read(cx)
                .vertical_scroll_handle
                .0
                .borrow()
                .base_handle
                .offset()
                .y
        })
    };
    let before = offset(&h, &vcx);
    let bottom = h.tile.read_with(&vcx, |t, cx| {
        let b = t
            .table
            .read(cx)
            .vertical_scroll_handle
            .0
            .borrow()
            .base_handle
            .bounds();
        gpui::point(b.center().x, b.bottom() - px(2.))
    });
    start_drag(&mut vcx, 0, bottom);
    let gap = drop_gap(&h, &vcx);
    vcx.executor().advance_clock(Duration::from_millis(400));
    vcx.run_until_parked();
    assert!(offset(&h, &vcx) < before, "scrolled down");
    assert!(drop_gap(&h, &vcx) > gap, "the gap follows the rows");
    up(&mut vcx, bottom, 1);
}
