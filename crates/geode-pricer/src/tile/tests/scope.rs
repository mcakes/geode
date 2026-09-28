//! The frame's scope over the pricer: applied in the frame observer
//! before the tile arrives at the flip barrier, re-applied on every
//! rebuild (edit, delivery), `:unscoped`, the header chips, a refused
//! scope's notice, the cursor off a hidden line, and a partly hidden
//! package's read-only row.

use super::*;
use geode_core::scope::{Scope, parse_expr};

/// Set the shared lane's scope expression through the frame entity, as
/// the scope bar does; the tile's observer runs on the notify.
fn set_expr(h: &Harness, vcx: &mut VisualTestContext, expr: &str) {
    let scope = Scope {
        expression: Some(parse_expr(expr).unwrap()),
        ..Default::default()
    };
    h.frame.update(vcx, |f, cx| {
        f.shared_mut().set_scope(scope);
        cx.notify();
    });
    h.draw(vcx);
}

fn hidden(h: &Harness, vcx: &VisualTestContext) -> usize {
    h.tile.read_with(vcx, |t, _| t.visibility.hidden)
}

fn painted(vcx: &mut VisualTestContext, selector: &'static str) -> bool {
    vcx.debug_bounds(selector).is_some()
}

fn footer_is_partly_hidden(h: &Harness, vcx: &VisualTestContext) {
    assert_eq!(
        h.footer(vcx).as_deref(),
        Some("package partly hidden by the scope: edit its legs")
    );
}

/// [A 5000 C, P(4800 C, 5200 C), B 4000 P] under `strike > 5000`: only
/// P's 5200 leg matches, so P shows (partly) and A, P's 4800 leg and B
/// hide. The observer applies the scope before it arrives: the barrier
/// opened for this tile's key is answered on the same notify.
#[gpui::test]
fn a_frame_scope_hides_lines_and_the_header_counts_them(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.draw(&mut vcx);
    assert!(!painted(&mut vcx, "pricer-hidden"), "absent at zero");
    let scope = Scope {
        expression: Some(parse_expr("strike > 5000").unwrap()),
        ..Default::default()
    };
    h.frame.update(&mut vcx, |f, cx| {
        f.shared_mut().set_scope(scope);
        f.shared_mut()
            .open_flip([QueryKey(TILE)], std::time::Instant::now());
        cx.notify();
    });
    assert!(
        !h.frame.read_with(&vcx, |f, _| f.barrier_open()),
        "the tile arrived"
    );
    assert_eq!(hidden(&h, &vcx), 3);
    assert_eq!(h.tree(&vcx).len(), 1, "P alone, closed");
    h.draw(&mut vcx);
    assert!(painted(&mut vcx, "pricer-hidden"));
    assert!(
        h.tile
            .read_with(&vcx, |t, _| t.header.hidden.as_deref() == Some("3 hidden")),
        "the chip counts hidden lines"
    );
    // An unrelated frame notify (same effective scope) changes nothing.
    h.frame.update(&mut vcx, |_, cx| cx.notify());
    assert_eq!(hidden(&h, &vcx), 3);
}

/// Open a flip for this tile's key, set `expr` and notify, as the scope
/// bar does; answers whether the tile arrived (the barrier closed).
fn flip_to(h: &Harness, vcx: &mut VisualTestContext, expr: &str) -> bool {
    let scope = Scope {
        expression: Some(parse_expr(expr).unwrap()),
        ..Default::default()
    };
    h.frame.update(vcx, |f, cx| {
        f.shared_mut().set_scope(scope);
        f.shared_mut()
            .open_flip([QueryKey(TILE)], std::time::Instant::now());
        cx.notify();
    });
    !h.frame.read_with(vcx, |f, _| f.barrier_open())
}

/// The tile arrives on the paths that apply nothing: a refused scope, and
/// a frame change while `:unscoped`.
#[gpui::test]
fn the_tile_arrives_on_a_refused_scope_and_while_unscoped(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    assert!(flip_to(&h, &mut vcx, "book = 'X'"), "refused: arrived");
    assert_eq!(hidden(&h, &vcx), 0);
    h.command(&mut vcx, "unscoped").unwrap();
    assert!(flip_to(&h, &mut vcx, "strike > 5000"), "unscoped: arrived");
    assert_eq!(hidden(&h, &vcx), 0);
}

#[gpui::test]
fn unscoped_shows_every_line_and_restores_from_the_session(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    set_expr(&h, &mut vcx, "strike > 5000");
    assert_eq!(hidden(&h, &vcx), 3);
    assert!(!painted(&mut vcx, "pricer-unscoped"));
    h.command(&mut vcx, "unscoped").unwrap();
    h.draw(&mut vcx);
    assert_eq!(hidden(&h, &vcx), 0);
    assert_eq!(h.tree(&vcx).len(), 3, "A, P, B");
    assert!(painted(&mut vcx, "pricer-unscoped"));
    assert!(!painted(&mut vcx, "pricer-hidden"));
    let record = h.serialize(&mut vcx);
    assert_eq!(record.get("unscoped").and_then(|v| v.as_bool()), Some(true));
    // A frame change while unscoped is not followed.
    set_expr(&h, &mut vcx, "strike < 4500");
    assert_eq!(hidden(&h, &vcx), 0);
    // Toggling back re-applies the frame's CURRENT scope at once.
    h.command(&mut vcx, "unscoped").unwrap();
    h.draw(&mut vcx);
    assert_eq!(
        hidden(&h, &vcx),
        3,
        "A, the 4800 leg and the 5200 leg hidden"
    );
    assert!(!painted(&mut vcx, "pricer-unscoped"));
    let record = h.serialize(&mut vcx);
    assert_ne!(record.get("unscoped").and_then(|v| v.as_bool()), Some(true));
}

/// A tile restored from a record carrying `unscoped = true` starts
/// unscoped: its frame's scope hides nothing and the chip paints.
#[gpui::test]
fn a_tile_restored_unscoped_ignores_the_frame_scope(cx: &mut gpui::TestAppContext) {
    let (store, mut record) = seeded(&BOOK);
    record.insert("unscoped".into(), toml::Value::Boolean(true));
    let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
    h.visible(&mut vcx, true);
    set_expr(&h, &mut vcx, "strike > 5000");
    assert_eq!(hidden(&h, &vcx), 0);
    assert!(painted(&mut vcx, "pricer-unscoped"));
    h.command(&mut vcx, "unscoped").unwrap();
    assert_eq!(hidden(&h, &vcx), 3, "re-attached at once");
}

#[gpui::test]
fn a_refused_scope_notices_and_hides_nothing(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    set_expr(&h, &mut vcx, "book = 'X'");
    assert_eq!(
        h.notice(&vcx).as_deref(),
        Some("scope refused: 'book' is not a pricer column")
    );
    assert_eq!(hidden(&h, &vcx), 0);
    assert_eq!(h.tree(&vcx).len(), 3);
    assert!(!painted(&mut vcx, "pricer-hidden"));
    // A delivery's rebuild keeps the refusal standing.
    let batch = h.prices().pop().expect("a price batch");
    h.answer(&mut vcx, &batch, 1.0);
    assert_eq!(
        h.notice(&vcx).as_deref(),
        Some("scope refused: 'book' is not a pricer column")
    );
    // A scope the pricer can honour withdraws it.
    set_expr(&h, &mut vcx, "strike > 5000");
    assert_eq!(h.notice(&vcx), None);
    assert_eq!(hidden(&h, &vcx), 3);
    // A refusal after a scope that hid lines shows them all again: the
    // pricer never keeps a narrowing the frame no longer holds.
    set_expr(&h, &mut vcx, "book = 'X'");
    assert_eq!(hidden(&h, &vcx), 0);
    assert_eq!(h.tree(&vcx).len(), 3);
}

/// The cursor on P's 5200 leg; a scope then hides that leg: the cursor
/// goes to the nearest shown row ABOVE (the 4800 leg), not to the row
/// that slid into its index (B).
#[gpui::test]
fn the_cursor_on_a_hidden_leg_moves_to_a_visible_row(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.motion(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "expand", None);
    h.motion(&mut vcx, "down", Some(2));
    assert_eq!(
        h.cursor(&vcx).map(|c| c.0),
        Some(3),
        "fixture: the 5200 leg"
    );
    set_expr(&h, &mut vcx, "strike != 5200");
    assert_eq!(h.tree(&vcx).len(), 4, "A, P, the 4800 leg, B");
    assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(2), "the 4800 leg");
    let line = h.tile.read_with(&vcx, |t, _| {
        t.cursor.line.and_then(|id| t.sheet.index_of(id))
    });
    assert_eq!(line, Some(2), "the cursor's line is the 4800 leg's");
}

/// `npv > 0` over unpriced lines hides them (NULL is not TRUE); the
/// price delivery's rebuild re-evaluates the scope and they show.
#[gpui::test]
fn a_delivery_reevaluates_the_scope(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "SPX Z26 4000 P"]);
    set_expr(&h, &mut vcx, "npv > 0");
    assert_eq!(hidden(&h, &vcx), 2, "unpriced: no npv");
    assert!(h.tree(&vcx).is_empty());
    let batch = h.prices().pop().expect("a price batch");
    h.answer(&mut vcx, &batch, 5.0);
    assert_eq!(hidden(&h, &vcx), 0);
    assert_eq!(h.tree(&vcx).len(), 2);
}

/// P under `strike != 5200` is partly hidden: its row paints the 4800
/// leg alone and is read-only. `i` on its strike and on its qty refuses
/// with the footer, and no edit applies.
#[gpui::test]
fn an_edit_on_a_partly_hidden_package_row_is_refused_with_the_footer(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    set_expr(&h, &mut vcx, "strike != 5200");
    h.motion(&mut vcx, "down", None);
    let before = h.tile.read_with(&vcx, |t, _| t.sheet.shorthand(1));
    for column in ["strike", "qty"] {
        goto_column(&h, &mut vcx, column);
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.mode(&mut vcx), "normal", "{column}: no editor opened");
        footer_is_partly_hidden(&h, &vcx);
    }
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.sheet.shorthand(1)), before);
    assert!(!h.tile.read_with(&vcx, |t, _| t.undo.can_undo()));
}

/// A typed commit over a `V` selection that includes the partly hidden
/// package would write its hidden leg too (a package stands for all its
/// legs): refused whole, nothing written.
#[gpui::test]
fn a_selection_commit_over_a_partly_hidden_package_is_refused(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    set_expr(&h, &mut vcx, "strike != 5200");
    h.motion(&mut vcx, "down", None);
    goto_column(&h, &mut vcx, "qty");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.motion(&mut vcx, "up", None); // the cursor on A, P still selected
    h.dispatch(&mut vcx, "edit", None);
    assert_eq!(h.mode(&mut vcx), "insert", "A's own cell opens");
    set_editor(&h, &mut vcx, "3");
    h.dispatch(&mut vcx, "commit", None);
    footer_is_partly_hidden(&h, &vcx);
    let (a, legs) = h.tile.read_with(&vcx, |t, _| {
        (
            t.sheet.qty(0),
            (2..4).map(|r| t.sheet.qty(r)).collect::<Vec<_>>(),
        )
    });
    assert_eq!(a, 1, "A unwritten");
    assert_eq!(legs, [-5, 5], "no leg written");
    assert!(!h.tile.read_with(&vcx, |t, _| t.undo.can_undo()));
}

/// The live step over the same selection: refused, nothing stepped.
#[gpui::test]
fn a_selection_step_over_a_partly_hidden_package_is_refused(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    set_expr(&h, &mut vcx, "strike != 5200");
    h.motion(&mut vcx, "down", None);
    goto_column(&h, &mut vcx, "strike");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.motion(&mut vcx, "up", None);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", None);
    footer_is_partly_hidden(&h, &vcx);
    let shorthand = |r: usize| h.tile.read_with(&vcx, |t, _| t.sheet.shorthand(r));
    assert_eq!(shorthand(0), "SPX Z26 5000 C", "A unstepped");
    assert_eq!(shorthand(1), "-5 SPX Z26 4800/5200 CS", "no leg stepped");
}

/// A `V` selection over the partly hidden package totals what its row
/// paints — the shown leg's value — never the full fold over the hidden
/// leg too.
#[gpui::test]
fn a_selection_total_over_a_partly_hidden_package_counts_its_shown_legs(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    let batch = h.prices().pop().expect("a price batch");
    h.answer(&mut vcx, &batch, 1.0);
    set_expr(&h, &mut vcx, "strike != 5200");
    h.motion(&mut vcx, "down", None);
    let painted = h.cell(&vcx, 1, "npv");
    assert_eq!(painted, "-5.00", "fixture: the 4800 leg alone, -5 × 1.00");
    h.dispatch(&mut vcx, "visual_rows", None);
    let total = h.tile.read_with(&vcx, |t, _| {
        t.totals
            .iter()
            .find(|c| c.label.as_ref() == "npv")
            .map(|c| c.text.to_string())
    });
    assert_eq!(total.as_deref(), Some("-5.00"));
}

// ---- structural verbs on a partly hidden package ----

/// The sheet's rows as shorthand, to prove a refused verb changed nothing.
fn rows(h: &Harness, vcx: &VisualTestContext) -> Vec<String> {
    h.tile.read_with(vcx, |t, _| {
        (0..t.sheet.len()).map(|r| t.sheet.shorthand(r)).collect()
    })
}

/// BOOK under `strike != 5200` with the cursor on P, partly hidden.
fn on_partial_package(cx: &mut gpui::TestAppContext) -> (Harness, VisualTestContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    set_expr(&h, &mut vcx, "strike != 5200");
    h.motion(&mut vcx, "down", None);
    assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(1), "fixture: on P");
    (h, vcx)
}

/// `d d`, `shift+j`/`shift+k` and `g u` on the partly hidden package row
/// refuse with the footer and change nothing: each would act on the
/// hidden leg too.
#[gpui::test]
fn row_verbs_on_a_partly_hidden_package_are_refused(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = on_partial_package(cx);
    let before = rows(&h, &vcx);
    for verb in ["delete", "move_down", "move_up", "ungroup"] {
        h.dispatch(&mut vcx, verb, None);
        footer_is_partly_hidden(&h, &vcx);
        assert_eq!(rows(&h, &vcx), before, "`{verb}` changed nothing");
    }
    // The command door refuses the same way.
    for line in ["ungroup", "group 2"] {
        assert_eq!(
            h.command(&mut vcx, line),
            Err("package partly hidden by the scope: edit its legs".to_string()),
            ":{line}"
        );
    }
    assert_eq!(rows(&h, &vcx), before);
    assert!(!h.tile.read_with(&vcx, |t, _| t.undo.can_undo()));
}

/// `g u` from the package's shown leg names the package too: refused.
/// The leg itself stays deletable.
#[gpui::test]
fn a_shown_leg_ungroups_nothing_but_deletes_itself(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = on_partial_package(cx);
    h.dispatch(&mut vcx, "expand", None);
    h.motion(&mut vcx, "down", None);
    assert_eq!(
        h.cursor(&vcx).map(|c| c.0),
        Some(2),
        "fixture: the 4800 leg"
    );
    let before = rows(&h, &vcx);
    h.dispatch(&mut vcx, "ungroup", None);
    footer_is_partly_hidden(&h, &vcx);
    assert_eq!(rows(&h, &vcx), before);
    h.dispatch(&mut vcx, "delete", None);
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.sheet.children(1).len()),
        1,
        "the shown leg deletes alone"
    );
}

/// `g p` with a count reaching the partly hidden package as a member.
#[gpui::test]
fn a_counted_group_reaching_a_partly_hidden_package_is_refused(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    set_expr(&h, &mut vcx, "strike != 5200");
    let before = rows(&h, &vcx);
    h.dispatch(&mut vcx, "group", Some(2));
    footer_is_partly_hidden(&h, &vcx);
    assert_eq!(rows(&h, &vcx), before);
}

/// A `V` selection holding the partly hidden package refuses delete and
/// move as a whole: no part of the selection is acted on.
#[gpui::test]
fn selection_verbs_over_a_partly_hidden_package_refuse_whole(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    set_expr(&h, &mut vcx, "strike != 5200");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.motion(&mut vcx, "bottom", None); // A, P, B
    let before = rows(&h, &vcx);
    for verb in ["delete", "move_up", "ungroup", "group"] {
        h.dispatch(&mut vcx, verb, None);
        footer_is_partly_hidden(&h, &vcx);
        assert_eq!(rows(&h, &vcx), before, "`{verb}` changed nothing");
    }
    assert!(!h.tile.read_with(&vcx, |t, _| t.undo.can_undo()));
    // Non-mutating verbs stay: a yank over the same selection works.
    h.dispatch(&mut vcx, "yank", None);
    assert!(h.tile.read_with(&vcx, |t, _| t.register.is_some()));
}

/// `2 g p` on A takes the next sheet row too; B is hidden by the scope,
/// so the package would hold a line the trader never saw: refused, by the
/// key and by `:group 2`.
#[gpui::test]
fn a_counted_group_over_a_hidden_line_is_refused(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "SPX Z26 4000 P", "SPX Z26 3000 P"]);
    set_expr(&h, &mut vcx, "strike != 4000");
    assert_eq!(h.tree(&vcx).len(), 2, "fixture: A and C shown");
    let before = rows(&h, &vcx);
    h.dispatch(&mut vcx, "group", Some(2));
    assert_eq!(
        h.footer(&vcx).as_deref(),
        Some("a line in that range is hidden by the scope")
    );
    assert_eq!(rows(&h, &vcx), before);
    assert_eq!(
        h.command(&mut vcx, "group 2"),
        Err("a line in that range is hidden by the scope".to_string())
    );
    assert_eq!(rows(&h, &vcx), before);
    assert!(!h.tile.read_with(&vcx, |t, _| t.undo.can_undo()));
}

/// A typed count of `usize::MAX` refuses through the command door: the
/// door's range and the core edit's end saturate instead of overflowing.
#[gpui::test]
fn a_huge_counted_group_refuses_without_overflowing(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "SPX Z26 4000 P", "SPX Z26 3000 P"]);
    // Off row 0, so `row + count` would overflow.
    h.motion(&mut vcx, "down", None);
    let before = rows(&h, &vcx);
    assert_eq!(
        h.command(&mut vcx, "group 18446744073709551615"),
        Err("group needs a contiguous run of top-level lines".to_string())
    );
    assert_eq!(rows(&h, &vcx), before);
    set_expr(&h, &mut vcx, "strike != 3000");
    assert_eq!(
        h.cursor(&vcx).map(|c| c.0),
        Some(1),
        "fixture: on the 4000 line"
    );
    assert_eq!(
        h.command(&mut vcx, "group 18446744073709551615"),
        Err("a line in that range is hidden by the scope".to_string())
    );
    assert!(!h.tile.read_with(&vcx, |t, _| t.undo.can_undo()));
}

/// A tile restored with its cursor on a line the frame's scope hides
/// lands on the nearest shown line above it, not on row 0.
#[gpui::test]
fn a_restored_cursor_on_a_hidden_line_lands_above_it(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    let (leg, package) = h
        .tile
        .read_with(&vcx, |t, _| (t.sheet.id(3), t.sheet.id(1)));
    set_expr(&h, &mut vcx, "strike != 5200");
    // Hand `book` back so a second tile may open it.
    h.command(&mut vcx, "new").unwrap();
    let mut record = toml::Table::new();
    record.insert("sheet".into(), "book".into());
    record.insert("cursor".into(), toml::Value::Integer(leg.0 as i64));
    record.insert(
        "expanded".into(),
        toml::Value::Array(vec![toml::Value::Integer(package.0 as i64)]),
    );
    let second = vcx.update(|window, cx| {
        h.factory.create(
            TileId(TILE + 1),
            Some(&record),
            FrameRef::new(h.frame.clone(), WorkspaceIx::FIRST),
            h.diagnostics.clone(),
            window,
            cx,
        )
    });
    let tile = second.view.downcast::<PricerTile>().unwrap();
    let at = tile.read_with(&vcx, |t, _| {
        assert_eq!(t.sheet.name, "book", "fixture: the restored sheet");
        t.cursor_sheet_row()
    });
    assert_eq!(at, Some(2), "the 4800 leg, above the hidden 5200 leg");
}
