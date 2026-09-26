//! The scope expression dialog (command-line locality spec §4.1): the
//! palette door, a commit through `Frame::set_scope` (so undo sees it),
//! the inline parse error, the empty commit that clears, the expression
//! chip's click (typing after the click must land — the mouse-opened
//! dialog rule), and focus returning to the text field.

use super::*;
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
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_some()));
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
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
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
    assert!(
        shell.read_with(&vcx, |s, _| s.modal.is_some()),
        "stays open"
    );
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
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
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
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
    assert!(
        filter_is_focused(&shell, &mut vcx),
        "focus went back to the field"
    );
}
