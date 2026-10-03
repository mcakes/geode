//! The Scope dialog: the lane's scope by ingredient (Current), with steps
//! for each ingredient pushed over it.

use super::*;
use geode_core::scope::{DimensionSelection, Scope, parse_expr};

use crate::shell::scopedialog::rows::RowId;

/// `book` and `desk` are dimensions; `npv` and `delta` measures; one
/// saved scope `eu` (book BK001) and one named expression `liq`.
pub(super) fn services() -> ShellServices {
    let mut services = test_services();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            LayerDoc::builtin(
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.desk]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
                 [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                 [risk.columns.delta]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
            )
            .unwrap(),
            LayerDoc::builtin("scopes", "[eu]\n[eu.dimensions]\nbook = [\"BK001\"]\n").unwrap(),
            LayerDoc::builtin("expressions", "[liq]\nexpression = \"npv > 0\"\n").unwrap(),
        ],
        ..ConfigSources::default()
    });
    services
}

pub(super) fn frame_of(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> Entity<Frame> {
    shell.read_with(cx, |s, _| s.frame().clone())
}

pub(super) fn draw(cx: &mut gpui::VisualTestContext) {
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

/// A shell whose shared lane holds `scope`, with the Scope dialog open
/// through its door and drawn once.
pub(super) fn open_on(
    cx: &mut gpui::TestAppContext,
    scope: Scope,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    let (window, mut vcx) = open_shell(cx, services());
    let shell = shell_of(&window, &mut vcx);
    frame_of(&shell, &vcx).update(&mut vcx, |f, cx| {
        f.shared_mut().set_scope(scope);
        cx.notify();
    });
    vcx.run_until_parked();
    dispatch_action(&shell, "frame::scope", &mut vcx);
    draw(&mut vcx);
    (shell, vcx)
}

pub(super) fn rich_scope() -> Scope {
    Scope {
        dimensions: vec![DimensionSelection {
            column: "book".into(),
            values: vec!["BK001".into(), "BK002".into()],
        }],
        text: Some("dec".into()),
        expression: Some(parse_expr("npv > 0 and delta < 5").unwrap()),
        impossible: false,
        named: vec!["liq".into()],
    }
}

#[gpui::test]
fn mod_o_opens_the_scope_dialog_on_current(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services());
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("alt-o");
    draw(&mut vcx);
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::Scope)
    );
    assert!(vcx.debug_bounds("scope-dialog").is_some());
    for section in ["dimensions", "expressions", "text"] {
        assert!(
            bounds(&mut vcx, format!("scope-dialog-section-{section}")).is_some(),
            "{section} header paints"
        );
        assert!(
            bounds(&mut vcx, format!("scope-dialog-empty-{section}")).is_some(),
            "{section} empty row paints"
        );
    }
}

#[gpui::test]
fn each_ingredient_is_a_painted_row_in_section_order(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, rich_scope());
    let ids = shell.read_with(&vcx, |s, _| {
        s.scope_dialog
            .as_ref()
            .unwrap()
            .rows
            .rows
            .iter()
            .map(|r| r.id.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(ids.len(), 5, "{ids:?}");
    for i in 0..5 {
        assert!(
            bounds(&mut vcx, format!("scope-dialog-row-{i}")).is_some(),
            "row {i} paints"
        );
    }
    assert!(vcx.debug_bounds("scope-dialog-empty-dimensions").is_none());
    let tops: Vec<_> = (0..5)
        .map(|i| {
            bounds(&mut vcx, format!("scope-dialog-row-{i}"))
                .unwrap()
                .top()
        })
        .collect();
    assert!(tops.windows(2).all(|w| w[0] < w[1]), "{tops:?}");
}

#[gpui::test]
fn the_title_says_where_the_scope_came_from(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, Scope::default());
    assert_eq!(
        title_extra(&shell, &vcx),
        None,
        "an empty scope says nothing"
    );
    assert!(vcx.debug_bounds("scope-dialog-title-extra").is_none());
    frame_of(&shell, &vcx).update(&mut vcx, |f, cx| {
        f.shared_mut().load_scope("eu").unwrap();
        cx.notify();
    });
    draw(&mut vcx);
    assert_eq!(title_extra(&shell, &vcx).as_deref(), Some("from eu"));
    assert!(vcx.debug_bounds("scope-dialog-title-extra").is_some());
    frame_of(&shell, &vcx).update(&mut vcx, |f, cx| {
        f.shared_mut().set_text(Some("x".into()));
        cx.notify();
    });
    draw(&mut vcx);
    assert_eq!(
        title_extra(&shell, &vcx).as_deref(),
        Some("from eu, changed")
    );
    assert!(vcx.debug_bounds("scope-dialog-title-extra").is_some());
}

#[gpui::test]
fn a_contradiction_paints_its_line(cx: &mut gpui::TestAppContext) {
    let (_shell, mut vcx) = open_on(
        cx,
        Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec![],
            }],
            impossible: true,
            ..Scope::default()
        },
    );
    assert!(vcx.debug_bounds("scope-dialog-contradiction").is_some());
}

#[gpui::test]
fn rows_follow_the_frame_while_open(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, Scope::default());
    frame_of(&shell, &vcx).update(&mut vcx, |f, cx| {
        f.shared_mut().set_text(Some("spx".into()));
        cx.notify();
    });
    vcx.run_until_parked();
    // A stale prepared list panics in `build` (debug); drawing proves it is current.
    draw(&mut vcx);
    assert!(vcx.debug_bounds("scope-dialog-row-0").is_some());
    assert!(vcx.debug_bounds("scope-dialog-empty-text").is_none());
}

#[gpui::test]
fn escape_closes_the_dialog(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, rich_scope());
    vcx.simulate_keystrokes("escape");
    assert_eq!(shell.read_with(&vcx, |s, _| s.top_kind()), None);
}

fn cursor_id(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> Option<RowId> {
    shell.read_with(cx, |s, _| {
        s.scope_dialog.as_ref().and_then(|d| d.cursor_id.clone())
    })
}

fn lane_scope(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> Scope {
    frame_of(shell, cx).read_with(cx, |f, _| f.shared().scope().clone())
}

#[gpui::test]
fn j_and_k_move_the_cursor_and_wrap(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, rich_scope());
    assert_eq!(
        cursor_id(&shell, &vcx),
        Some(RowId::Dimension("book".into()))
    );
    vcx.simulate_keystrokes("j j");
    assert_eq!(
        cursor_id(&shell, &vcx),
        Some(RowId::Term("delta < 5".into(), 0))
    );
    vcx.simulate_keystrokes("k k k");
    assert_eq!(
        cursor_id(&shell, &vcx),
        Some(RowId::Text),
        "k from the top wraps"
    );
}

#[gpui::test]
fn d_removes_the_cursor_row_in_one_undo_step(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, rich_scope());
    vcx.simulate_keystrokes("d");
    assert!(
        lane_scope(&shell, &vcx)
            .dimensions
            .iter()
            .all(|d| d.values.is_empty())
    );
    vcx.simulate_keystrokes("j d"); // now on the second term
    let scope = lane_scope(&shell, &vcx);
    assert_eq!(scope.expression, Some(parse_expr("npv > 0").unwrap()));
    // The removed term's index now holds the named reference.
    vcx.simulate_keystrokes("d");
    assert!(lane_scope(&shell, &vcx).named.is_empty());
    vcx.simulate_keystrokes("d"); // the text, now under the cursor
    assert_eq!(lane_scope(&shell, &vcx).text, None);
    vcx.simulate_keystrokes("u");
    assert_eq!(lane_scope(&shell, &vcx).text.as_deref(), Some("dec"));
    vcx.simulate_keystrokes("ctrl-r");
    assert_eq!(lane_scope(&shell, &vcx).text, None);
    draw(&mut vcx);
}

#[gpui::test]
fn shift_d_clears_the_whole_scope(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, rich_scope());
    vcx.simulate_keystrokes("shift-d");
    assert!(lane_scope(&shell, &vcx).is_empty());
    draw(&mut vcx);
    assert!(vcx.debug_bounds("scope-dialog-empty-dimensions").is_some());
}

#[gpui::test]
fn the_cursor_stays_on_its_row_when_another_row_goes(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, rich_scope());
    vcx.simulate_keystrokes("j j j"); // the named reference
    frame_of(&shell, &vcx).update(&mut vcx, |f, cx| {
        f.shared_mut().drop_dimension("book");
        cx.notify();
    });
    vcx.run_until_parked();
    draw(&mut vcx);
    assert_eq!(cursor_id(&shell, &vcx), Some(RowId::Named("liq".into())));
}

#[gpui::test]
fn removing_a_term_that_moved_refuses(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, rich_scope());
    vcx.simulate_keystrokes("j j"); // term 1: delta < 5
    // The rows are derived; swap the expression without letting them refresh,
    // so the row's index and seed are stale when `d` runs.
    frame_of(&shell, &vcx).update(&mut vcx, |f, _| {
        let mut s = f.shared().scope().clone();
        s.expression = Some(parse_expr("npv > 0 and desk = 'EQ'").unwrap());
        f.shared_mut().set_scope(s);
    });
    vcx.simulate_keystrokes("d");
    assert_eq!(
        lane_scope(&shell, &vcx).expression,
        Some(parse_expr("npv > 0 and desk = 'EQ'").unwrap()),
        "a stale term row must not remove whichever term now has its index"
    );
    let error = shell.read_with(&vcx, |s, _| s.scope_dialog.as_ref().unwrap().error.clone());
    assert!(error.is_some());
}

#[gpui::test]
fn the_dialog_edits_the_lane_it_was_opened_on(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services());
    let shell = shell_of(&window, &mut vcx);
    let ws2 = crate::tiling::WorkspaceIx::new(2).unwrap();
    dispatch_action(&shell, "workspace::switch_2", &mut vcx);
    vcx.run_until_parked();
    frame_of(&shell, &vcx).update(&mut vcx, |f, cx| {
        assert!(f.pin(ws2));
        f.view_mut(ws2).set_text(Some("pinned".into()));
        cx.notify();
    });
    dispatch_action(&shell, "frame::scope", &mut vcx);
    draw(&mut vcx);
    vcx.simulate_keystrokes("d");
    let frame = frame_of(&shell, &vcx);
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.view(ws2).scope().text.clone()),
        None
    );
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().scope().clone()),
        Scope::default()
    );
}

fn title_extra(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> Option<String> {
    shell.read_with(cx, |s, _| {
        s.scope_dialog
            .as_ref()
            .and_then(|d| d.title.as_ref().map(|t| t.to_string()))
    })
}

/// `debug_bounds` takes a `'static` selector; tests leak the few they format.
fn bounds(
    vcx: &mut gpui::VisualTestContext,
    selector: String,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    vcx.debug_bounds(Box::leak(selector.into_boxed_str()))
}
