//! Scope-expression integration: opening from the palette, committing through
//! `Frame::set_scope` for undo support, parse errors, empty-expression clearing, chip
//! clicks, and restoring focus to the text field.

use super::*;
use crate::shell::EXPR_KEY;
use geode_core::query::{DistinctOutcome, DistinctParams};
use geode_core::scope::{Scope, parse_expr};

fn expr_scope(text: &str) -> Scope {
    Scope {
        expression: Some(parse_expr(text).unwrap()),
        ..Scope::default()
    }
}

/// `enter` on a typed expression commits it through `Frame::set_scope`,
/// closes the dialog, and — since it went through `set_scope` rather
/// than bypassing undo — `frame::scope_undo` restores the prior scope.
#[gpui::test]
fn typing_an_expression_and_enter_sets_it_through_set_scope(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "frame::scope_expression");
    assert!(shell.read_with(&vcx, |s, _| s.modal_open()));
    assert!(dialog_filter_is_focused(&shell, &mut vcx));
    vcx.simulate_input("book = 'BK000'");
    vcx.simulate_keystrokes("enter");
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert_eq!(
        frame.read_with(&vcx, |f, _| f
            .scope()
            .expression
            .as_ref()
            .map(ToString::to_string)),
        Some("book = 'BK000'".to_string())
    );
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
    dispatch_action(&shell, "frame::scope_undo", &mut vcx);
    assert!(
        frame.read_with(&vcx, |f, _| f.scope().expression.is_none()),
        "the commit went through set_scope, so undo restores"
    );
}

/// An unparseable expression paints inline (`scope-expr-error`), the
/// modal stays open and the frame is untouched; typing again clears the
/// error.
#[gpui::test]
fn a_parse_error_paints_inline_and_leaves_the_frame_alone(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "frame::scope_expression");
    vcx.simulate_input("book =");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.modal_open()), "stays open");
    assert!(vcx.debug_bounds("scope-expr-error").is_some());
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert!(frame.read_with(&vcx, |f, _| f.scope().expression.is_none()));
    // Typing again clears the error.
    vcx.simulate_input(" 'x'");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-expr-error").is_none());
}

/// The field opens seeded with the frame's current expression text, and
/// committing it emptied (`enter` on a blank field) clears the
/// expression rather than refusing.
#[gpui::test]
fn the_field_opens_seeded_and_an_empty_commit_clears(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        f.set_scope(expr_scope("book = 'BK000'"));
        cx.notify();
    });
    dispatch_action(&shell, "frame::scope_expression", &mut vcx);
    assert_eq!(
        shell.read_with(&vcx, |s, cx| s.dialog_input.read(cx).value().to_string()),
        "book = 'BK000'",
        "seeded with the frame's expression"
    );
    vcx.update(|window, cx| {
        let input = shell.read(cx).dialog_input.clone();
        input.update(cx, |i, cx| i.set_value("", window, cx));
    });
    vcx.simulate_keystrokes("enter");
    assert!(frame.read_with(&vcx, |f, _| f.scope().expression.is_none()));
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
}

/// A click on the toolbar's expression chip opens the same dialog as
/// `frame::scope_expression`, and — the mouse-opened-dialog rule —
/// typing right after the click reaches the field rather than falling
/// on the floor.
#[gpui::test]
fn clicking_the_expression_chip_opens_the_dialog_and_typing_lands(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        f.set_scope(expr_scope("book = 'BK000'"));
        cx.notify();
    });
    vcx.run_until_parked();
    let chip = vcx
        .debug_bounds("scope-expr-chip-0")
        .expect("the expression chip paints");
    vcx.simulate_click(chip.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.scope_expr_dialog.is_some()));
    vcx.simulate_input(" and lhu = 'L1'");
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&vcx, |s, cx| s.dialog_input.read(cx).value().to_string()),
        "book = 'BK000' and lhu = 'L1'",
        "typing after the click reaches the field"
    );
}

/// Opening the dialog from the scope bar's focused text field and then
/// closing it (`escape`) returns focus to that field, not the shell
/// root.
#[gpui::test]
fn opened_from_the_text_field_focus_returns_to_it(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    dispatch_action(&shell, "frame::focus_text", &mut vcx);
    assert!(filter_is_focused(&shell, &mut vcx));
    dispatch_action(&shell, "frame::scope_expression", &mut vcx);
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
    assert!(
        filter_is_focused(&shell, &mut vcx),
        "focus went back to the field"
    );
}

/// `test_services` with a schema: `book` (categorical), `npv` (measure),
/// `live` (a bool attribute: the schema refuses a bool dimension), plus
/// the derived `desk`. The keymap doc is re-added because the config is
/// rebuilt from these layers alone.
fn services_with_schema() -> ShellServices {
    let mut services = test_services();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
         [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
         [risk.columns.live]\ntype = \"bool\"\nrole = \"attribute\"\ngrain = \"position\"\n",
    )
    .unwrap();
    let dims = LayerDoc::builtin(
        "dimensions",
        "[desk]\nfrom = \"book\"\n[desk.values]\nEQ = [\"BK000\"]\n",
    )
    .unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            datasets,
            dims,
        ],
        desk: None,
        user: None,
    });
    services
}

fn requests(
    shell: &Entity<ShellView>,
    vcx: &mut gpui::VisualTestContext,
) -> std::rc::Rc<std::cell::RefCell<Vec<DistinctParams>>> {
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    vcx.update(|_, cx| {
        let seen = seen.clone();
        cx.subscribe(shell, move |_, e: &ShellEvent, _| {
            if let ShellEvent::DistinctRequested(p) = e {
                seen.borrow_mut().push(p.clone());
            }
        })
        .detach();
    });
    seen
}

fn field(shell: &Entity<ShellView>, vcx: &gpui::VisualTestContext) -> String {
    shell.read_with(vcx, |s, cx| s.dialog_input.read(cx).value().to_string())
}

/// The empty field lists columns; tab inserts the highlighted one and the
/// list moves on to its operators.
#[gpui::test]
fn tab_inserts_a_column_and_the_list_moves_to_operators(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("scope-expr-row-book").is_some(),
        "columns listed"
    );
    vcx.simulate_input("np");
    vcx.simulate_keystrokes("tab");
    vcx.run_until_parked();
    assert_eq!(field(&shell, &vcx), "npv ");
    assert!(
        vcx.debug_bounds("scope-expr-row->=").is_some(),
        "number operators"
    );
    assert!(vcx.debug_bounds("scope-expr-row-like").is_none());
    assert!(
        dialog_filter_is_focused(&shell, &mut vcx),
        "tab never leaves the field"
    );
}

/// down moves the highlight; tab then inserts that row.
#[gpui::test]
fn arrows_move_the_highlight_that_tab_inserts(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    vcx.simulate_input("npv ");
    vcx.simulate_keystrokes("down down tab");
    vcx.run_until_parked();
    assert_eq!(field(&shell, &vcx), "npv < ");
    vcx.simulate_keystrokes("shift-tab");
    vcx.run_until_parked();
    assert_eq!(
        field(&shell, &vcx),
        "npv < ",
        "shift-tab moves, never writes"
    );
}

/// A value position requests the column's values once, scoped as the
/// mode says. A delivery through `deliver_distinct` fills the rows, and a
/// stale tag is ignored.
#[gpui::test]
fn values_arrive_through_the_distinct_path_and_stale_replies_are_dropped(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    let seen = requests(&shell, &mut vcx);
    vcx.simulate_input("book = ");
    vcx.run_until_parked();
    vcx.simulate_input("'");
    vcx.run_until_parked();
    let req = seen.borrow().last().cloned().expect("a request");
    assert_eq!(seen.borrow().len(), 1, "asked once");
    assert_eq!((req.key, req.column.as_str()), (EXPR_KEY, "book"));
    let deliver = |shell: &Entity<ShellView>, vcx: &mut gpui::VisualTestContext, tag| {
        shell.update(vcx, |s, cx| {
            s.deliver_distinct(
                DistinctOutcome {
                    key: EXPR_KEY,
                    tag,
                    column: "book".into(),
                    values: Ok(vec![("EMEA".into(), 12)]),
                },
                cx,
            )
        });
        vcx.run_until_parked();
    };
    deliver(&shell, &mut vcx, req.tag.wrapping_sub(1));
    assert!(
        vcx.debug_bounds("scope-expr-row-'EMEA'").is_none(),
        "stale reply dropped"
    );
    deliver(&shell, &mut vcx, req.tag);
    assert!(vcx.debug_bounds("scope-expr-row-'EMEA'").is_some());
    vcx.simulate_keystrokes("tab");
    vcx.run_until_parked();
    assert_eq!(field(&shell, &vcx), "book = 'EMEA'");
}

/// A reply that arrives after the dialog closed does nothing.
#[gpui::test]
fn a_reply_after_close_is_ignored(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    let seen = requests(&shell, &mut vcx);
    vcx.simulate_input("book = ");
    vcx.run_until_parked();
    let tag = seen.borrow().last().unwrap().tag;
    vcx.simulate_keystrokes("escape");
    shell.update(&mut vcx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: EXPR_KEY,
                tag,
                column: "book".into(),
                values: Ok(vec![]),
            },
            cx,
        )
    });
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
}

/// Add mode narrows by the frame's current expression.
#[gpui::test]
fn add_mode_requests_values_under_the_current_expression(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_schema());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        f.set_scope(expr_scope("live = true"));
        cx.notify();
    });
    let seen = requests(&shell, &mut vcx);
    dispatch_action(&shell, "frame::add_expression", &mut vcx);
    vcx.simulate_input("book = ");
    vcx.run_until_parked();
    let req = seen.borrow().last().cloned().expect("a request");
    assert_eq!(
        req.scope.expression.map(|e| e.to_string()),
        Some("live = true".to_string())
    );
}

/// A caret moved by a key, with no typing, re-reads the position. Tab
/// then writes at the new caret, not at the stale range.
#[gpui::test]
fn a_moved_caret_is_followed_before_tab_writes(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    vcx.simulate_input("npv > 1 and live ");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("scope-expr-row-=").is_some(),
        "bool operators at the end"
    );
    assert!(vcx.debug_bounds("scope-expr-row-book").is_none());
    vcx.simulate_keystrokes("home");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("scope-expr-row-book").is_some(),
        "the column list at the start"
    );
    vcx.simulate_keystrokes("tab");
    vcx.run_until_parked();
    assert_eq!(
        field(&shell, &vcx),
        "book npv > 1 and live ",
        "written at the new caret"
    );
}

/// cmd-z takes an insertion back, which proves the write kept undo.
#[gpui::test]
fn undo_takes_an_insertion_back(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    vcx.simulate_input("np");
    vcx.simulate_keystrokes("tab");
    vcx.run_until_parked();
    assert_eq!(field(&shell, &vcx), "npv ");
    vcx.simulate_keystrokes("cmd-z");
    vcx.run_until_parked();
    assert_eq!(field(&shell, &vcx), "np");
}

/// A row click inserts it and the field keeps the keyboard.
#[gpui::test]
fn clicking_a_row_inserts_it_and_typing_continues(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    vcx.run_until_parked();
    let bounds = vcx.debug_bounds("scope-expr-row-npv").expect("row painted");
    vcx.simulate_click(bounds.center(), gpui::Modifiers::none());
    vcx.run_until_parked();
    assert_eq!(field(&shell, &vcx), "npv ");
    vcx.simulate_input(">");
    assert_eq!(field(&shell, &vcx), "npv >");
}

/// A double-click on a row inserts it once: the second press lands on the
/// list the first accept rebuilt (operators), where `npv` is not a row.
#[gpui::test]
fn double_clicking_a_row_inserts_it_once(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    vcx.run_until_parked();
    let bounds = vcx.debug_bounds("scope-expr-row-npv").expect("row painted");
    double_click(&mut vcx, bounds.center(), gpui::Modifiers::none());
    vcx.run_until_parked();
    assert_eq!(field(&shell, &vcx), "npv ");
}

/// A row that survives its own accept (`(` opens a group, whose list
/// offers `(` again) still inserts once on a double-click: the second
/// press, `click_count` 2, is not a second accept.
#[gpui::test]
fn double_clicking_a_row_that_stays_listed_inserts_it_once(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    vcx.run_until_parked();
    let bounds = vcx.debug_bounds("scope-expr-row-(").expect("row painted");
    double_click(&mut vcx, bounds.center(), gpui::Modifiers::none());
    vcx.run_until_parked();
    assert_eq!(field(&shell, &vcx), "(");
}

/// Enter refuses an unknown column inline, and the text stays.
#[gpui::test]
fn enter_refuses_an_unknown_column(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    vcx.simulate_input("bokk = 'A'");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.modal_open()));
    assert_eq!(
        shell.read_with(&vcx, |s, _| s
            .scope_expr_dialog
            .as_ref()
            .and_then(|d| d.error.clone())),
        Some("unknown column 'bokk'; did you mean 'book'?".to_string())
    );
    assert!(
        vcx.debug_bounds("scope-expr-warning").is_some(),
        "live warning painted too"
    );
}

/// A hot reload that adds a column while the dialog is open is suggested
/// straight away.
#[gpui::test]
fn a_reload_rebuilds_the_vocab_under_an_open_dialog(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-expr-row-region").is_none());
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.region]\ntype = \"utf8\"\nrole = \"dimension\"\ngrain = \"position\"\n",
    )
    .unwrap();
    let (config, _) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            datasets,
        ],
        desk: None,
        user: None,
    });
    shell.update(&mut vcx, |s, cx| s.apply_reload(config, cx));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-expr-row-region").is_some());
}
