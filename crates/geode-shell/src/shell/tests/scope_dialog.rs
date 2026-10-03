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
    draw(&mut vcx);
    assert!(
        vcx.debug_bounds("scope-dialog-error").is_some(),
        "the refusal paints"
    );
    // The error describes the last action only: the next key clears it.
    vcx.simulate_keystrokes("j");
    draw(&mut vcx);
    assert!(vcx.debug_bounds("scope-dialog-error").is_none());
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

#[gpui::test]
fn a_picker_commit_returns_to_current_with_the_new_row(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, Scope::default());
    vcx.simulate_keystrokes("p");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::Picker)
    );
    // Pick a column, then a value from a delivered list.
    shell.update(&mut vcx, |s, cx| {
        let p = s.picker.as_mut().unwrap();
        p.stage = crate::shell::picker::Stage::Values {
            column: "book".into(),
        };
        p.values = Some(Ok(vec![("BK001".into(), 3)]));
        cx.notify();
    });
    vcx.simulate_keystrokes("enter");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::Scope)
    );
    assert_eq!(lane_scope(&shell, &vcx).sole("book"), Some("BK001"));
    draw(&mut vcx);
    assert!(vcx.debug_bounds("scope-dialog-row-0").is_some());
}

#[gpui::test]
fn mod_p_alone_still_closes_on_commit(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services());
    let shell = shell_of(&window, &mut vcx);
    dispatch_action(&shell, "frame::pick", &mut vcx);
    shell.update(&mut vcx, |s, cx| {
        let p = s.picker.as_mut().unwrap();
        p.stage = crate::shell::picker::Stage::Values {
            column: "book".into(),
        };
        p.values = Some(Ok(vec![("BK001".into(), 3)]));
        cx.notify();
    });
    vcx.simulate_keystrokes("enter");
    assert_eq!(shell.read_with(&vcx, |s, _| s.top_kind()), None);
}

#[gpui::test]
fn x_adds_an_expression_and_returns(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, Scope::default());
    vcx.simulate_keystrokes("x");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::ScopeExpr)
    );
    vcx.simulate_input("npv > 1");
    vcx.simulate_keystrokes("enter");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::Scope)
    );
    assert_eq!(
        lane_scope(&shell, &vcx).expression,
        Some(parse_expr("npv > 1").unwrap())
    );
    draw(&mut vcx);
    assert!(vcx.debug_bounds("scope-dialog-row-0").is_some());
}

#[gpui::test]
fn enter_on_a_term_edits_that_term(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, rich_scope());
    vcx.simulate_keystrokes("j enter"); // term 0: npv > 0
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::ScopeExpr)
    );
    // The idiom shell/tests/scope_expr.rs uses: the commit reads the field.
    vcx.update(|window, cx| {
        let input = shell.read(cx).dialog_input.clone();
        input.update(cx, |i, cx| i.set_value("npv > 9", window, cx));
    });
    vcx.simulate_keystrokes("enter");
    assert_eq!(
        lane_scope(&shell, &vcx).expression,
        Some(parse_expr("npv > 9 and delta < 5").unwrap())
    );
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::Scope)
    );
    draw(&mut vcx);
}

#[gpui::test]
fn e_on_a_dimension_opens_the_picker_on_its_column(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, rich_scope());
    vcx.simulate_keystrokes("e"); // row 0: book
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::Picker)
    );
    let stage = shell.read_with(&vcx, |s, _| s.picker.as_ref().map(|p| p.stage.clone()));
    assert_eq!(
        stage,
        Some(crate::shell::picker::Stage::Values {
            column: "book".into()
        })
    );
}

#[gpui::test]
fn enter_on_a_reference_opens_its_expressions_entry(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, rich_scope());
    vcx.simulate_keystrokes("j j j enter"); // the reference `liq`
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::Object)
    );
    let opened = shell.read_with(&vcx, |s, _| {
        s.object_dialog
            .as_ref()
            .map(|d| (d.domain, d.stage.clone()))
    });
    assert_eq!(
        opened,
        Some((
            crate::shell::objectdialog::Domain::Expressions,
            crate::shell::objectdialog::Stage::Edit {
                object: "liq".into()
            }
        ))
    );
    // Escape steps the object dialog back to its list, then closes it.
    vcx.simulate_keystrokes("escape escape");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::Scope)
    );
    draw(&mut vcx);
    assert!(vcx.debug_bounds("scope-dialog").is_some());
}

#[gpui::test]
fn a_row_double_click_acts_as_enter(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, rich_scope());
    let row = vcx
        .debug_bounds("scope-dialog-row-1")
        .expect("term row paints");
    super::double_click(&mut vcx, row.center(), gpui::Modifiers::default());
    assert_eq!(
        cursor_id(&shell, &vcx),
        Some(RowId::Term("npv > 0".into(), 0))
    );
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::ScopeExpr)
    );
    assert_eq!(shell.read_with(&vcx, |s, _| s.modal_depth()), 2);
}

#[gpui::test]
fn a_single_row_press_only_moves_the_cursor(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, rich_scope());
    let row = vcx
        .debug_bounds("scope-dialog-row-1")
        .expect("term row paints");
    vcx.simulate_click(row.center(), gpui::Modifiers::default());
    assert_eq!(
        cursor_id(&shell, &vcx),
        Some(RowId::Term("npv > 0".into(), 0))
    );
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::Scope)
    );
}

#[gpui::test]
fn i_inlines_a_reference_and_refuses_elsewhere(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, rich_scope());
    vcx.simulate_keystrokes("i");
    let error = shell.read_with(&vcx, |s, _| s.scope_dialog.as_ref().unwrap().error.clone());
    assert_eq!(
        error.as_deref(),
        Some(crate::shell::scopedialog::view::INLINE_ONLY_NAMED)
    );
    draw(&mut vcx);
    assert!(
        vcx.debug_bounds("scope-dialog-error").is_some(),
        "the refusal paints"
    );
    vcx.simulate_keystrokes("j j j i"); // the reference
    let scope = lane_scope(&shell, &vcx);
    assert!(scope.named.is_empty());
    assert_eq!(
        scope.expression,
        Some(parse_expr("npv > 0 and delta < 5 and npv > 0").unwrap())
    );
    draw(&mut vcx);
    assert!(vcx.debug_bounds("scope-dialog-error").is_none());
}

#[gpui::test]
fn mod_s_on_a_term_opens_its_name_entry(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, rich_scope());
    vcx.simulate_keystrokes("alt-s");
    let error = shell.read_with(&vcx, |s, _| s.scope_dialog.as_ref().unwrap().error.clone());
    assert_eq!(
        error.as_deref(),
        Some(crate::shell::scopedialog::view::NAME_ONLY_TERMS),
        "a dimension cannot be named"
    );
    draw(&mut vcx);
    assert!(
        vcx.debug_bounds("scope-dialog-error").is_some(),
        "the refusal paints"
    );
    vcx.simulate_keystrokes("j alt-s");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::ScopeExpr)
    );
    let naming = shell.read_with(&vcx, |s, _| {
        s.scope_expr_dialog.as_ref().and_then(|d| d.naming.clone())
    });
    assert_eq!(naming.as_deref(), Some("npv > 0"));
}

#[gpui::test]
fn o_and_s_push_the_saved_chooser_and_the_save_prompt(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, rich_scope());
    vcx.simulate_keystrokes("o");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::Choice)
    );
    vcx.simulate_keystrokes("enter"); // loads `eu`
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::Scope)
    );
    assert_eq!(lane_scope(&shell, &vcx).sole("book"), Some("BK001"));
    draw(&mut vcx);
    vcx.simulate_keystrokes("s");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::Object)
    );
    let domain = shell.read_with(&vcx, |s, _| s.object_dialog.as_ref().map(|d| d.domain));
    assert_eq!(domain, Some(crate::shell::objectdialog::Domain::Scopes));
}

/// Cursor on term 1 (`delta < 5`), then the lane's expression swapped
/// without letting the rows refresh, so the row's index now holds a
/// different term (`desk = 'EQ'`).
fn on_a_term_that_moved(
    cx: &mut gpui::TestAppContext,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    let (shell, mut vcx) = open_on(cx, rich_scope());
    vcx.simulate_keystrokes("j j");
    frame_of(&shell, &vcx).update(&mut vcx, |f, _| {
        let mut s = f.shared().scope().clone();
        s.expression = Some(parse_expr("npv > 0 and desk = 'EQ'").unwrap());
        f.shared_mut().set_scope(s);
    });
    (shell, vcx)
}

fn assert_term_gone_refusal(shell: &Entity<ShellView>, vcx: &mut gpui::VisualTestContext) {
    assert_eq!(
        shell.read_with(vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::Scope),
        "a stale term row must not open its neighbour"
    );
    let error = shell.read_with(vcx, |s, _| s.scope_dialog.as_ref().unwrap().error.clone());
    assert_eq!(
        error.as_deref(),
        Some(crate::shell::scope_expr_view::TERM_GONE)
    );
    draw(vcx);
    assert!(
        vcx.debug_bounds("scope-dialog-error").is_some(),
        "the refusal paints"
    );
}

#[gpui::test]
fn editing_a_term_that_moved_refuses(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = on_a_term_that_moved(cx);
    vcx.simulate_keystrokes("enter");
    assert_term_gone_refusal(&shell, &mut vcx);
}

#[gpui::test]
fn naming_a_term_that_moved_refuses(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = on_a_term_that_moved(cx);
    vcx.simulate_keystrokes("alt-s");
    assert_term_gone_refusal(&shell, &mut vcx);
    assert!(
        shell.read_with(&vcx, |s, _| s.scope_expr_dialog.is_none()),
        "no name entry opened for the neighbour"
    );
}

#[gpui::test]
fn t_types_the_text_filter_and_returns(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, Scope::default());
    vcx.simulate_keystrokes("t");
    draw(&mut vcx);
    assert!(vcx.debug_bounds("scope-dialog-text-field").is_some());
    // The step's footer replaces Current's.
    assert!(vcx.debug_bounds("scope-dialog-hint-set-text").is_some());
    assert!(vcx.debug_bounds("scope-dialog-hint-edit-row").is_none());
    // Surrounding blanks are not part of the filter.
    vcx.simulate_input("  spx  ");
    vcx.simulate_keystrokes("enter");
    assert_eq!(lane_scope(&shell, &vcx).text.as_deref(), Some("spx"));
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::Scope)
    );
    draw(&mut vcx);
    assert!(vcx.debug_bounds("scope-dialog-text-field").is_none());
    assert!(vcx.debug_bounds("scope-dialog-hint-edit-row").is_some());
    assert!(
        vcx.debug_bounds("scope-dialog-row-0").is_some(),
        "the text row paints"
    );
}

#[gpui::test]
fn keys_current_claims_type_into_the_text_step(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, rich_scope());
    vcx.simulate_keystrokes("t");
    // `d`, `j`, `x` are Current's verbs; inside the step they are text.
    vcx.simulate_keystrokes("d j x");
    let typed = shell.read_with(&vcx, |s, cx| s.dialog_input.read(cx).value().to_string());
    assert_eq!(typed, "decdjx");
    assert_eq!(lane_scope(&shell, &vcx), rich_scope(), "no verb ran");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::Scope)
    );
}

#[gpui::test]
fn enter_on_the_text_row_edits_it_and_empty_clears(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(
        cx,
        Scope {
            text: Some("dec".into()),
            ..Scope::default()
        },
    );
    vcx.simulate_keystrokes("enter");
    let seeded = shell.read_with(&vcx, |s, cx| s.dialog_input.read(cx).value().to_string());
    assert_eq!(seeded, "dec");
    draw(&mut vcx);
    assert!(vcx.debug_bounds("scope-dialog-text-field").is_some());
    vcx.simulate_keystrokes("backspace backspace backspace enter");
    assert_eq!(lane_scope(&shell, &vcx).text, None);
    draw(&mut vcx);
    assert!(vcx.debug_bounds("scope-dialog-text-field").is_none());
    assert!(vcx.debug_bounds("scope-dialog-empty-text").is_some());
}

#[gpui::test]
fn escape_leaves_the_text_step_without_a_change(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(
        cx,
        Scope {
            text: Some("dec".into()),
            ..Scope::default()
        },
    );
    vcx.simulate_keystrokes("t");
    vcx.simulate_input("x");
    let typed = shell.read_with(&vcx, |s, cx| s.dialog_input.read(cx).value().to_string());
    assert_eq!(typed, "decx", "the step's field took the text");
    vcx.simulate_keystrokes("escape");
    assert_eq!(lane_scope(&shell, &vcx).text.as_deref(), Some("dec"));
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.modal_depth()),
        1,
        "escape left the step, not a dialog pushed over it"
    );
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::Scope)
    );
    draw(&mut vcx);
    assert!(vcx.debug_bounds("scope-dialog-text-field").is_none());
    vcx.simulate_keystrokes("escape");
    assert_eq!(shell.read_with(&vcx, |s, _| s.top_kind()), None);
}
