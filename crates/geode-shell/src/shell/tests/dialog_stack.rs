//! Stacked dialogs through production routes. A dialog opened over another
//! pushes; Enter or Escape pops one level, and the revealed dialog has its
//! query, caret, mode and focus back. One instance per kind.

use super::*;
use crate::dialogmode::DialogMode;
use crate::shell::dialog::DialogKind;
use geode_core::query::DistinctOutcome;

fn input_text(shell: &Entity<ShellView>, cx: &mut gpui::VisualTestContext) -> String {
    shell.read_with(cx, |s, cx| s.dialog_input.read(cx).value().to_string())
}

fn input_cursor(shell: &Entity<ShellView>, cx: &mut gpui::VisualTestContext) -> usize {
    shell.read_with(cx, |s, cx| s.dialog_input.read(cx).cursor())
}

fn kinds(shell: &Entity<ShellView>, cx: &mut gpui::VisualTestContext) -> Vec<DialogKind> {
    shell.read_with(cx, |s, _| s.modals.iter().map(|m| m.kind).collect())
}

fn draw(cx: &mut gpui::VisualTestContext) {
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

/// Views in filter mode with "ab" typed, then Settings pushed through dispatch.
/// Settings owns the shared input while on top, and typing goes to it, not to the
/// hidden Views. Escape pops Settings only, and Views has its query, caret, mode
/// and focus back.
#[gpui::test]
fn a_pushed_dialog_owns_the_shared_input_until_it_pops(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "config::views");
    vcx.simulate_keystrokes("/ a b");
    assert_eq!(input_text(&shell, &mut vcx), "ab");

    dispatch_action(&shell, "settings::open", &mut vcx);
    draw(&mut vcx);
    assert_eq!(
        kinds(&shell, &mut vcx),
        vec![DialogKind::Object, DialogKind::Settings]
    );
    assert_eq!(
        input_text(&shell, &mut vcx),
        "",
        "the pushed dialog starts with a clear input"
    );

    vcx.simulate_keystrokes("/ x");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s
            .settings
            .as_ref()
            .unwrap()
            .effective_query()
            .to_string()),
        "x"
    );
    assert_eq!(
        shell.read_with(&vcx, |s, _| s
            .object_dialog
            .as_ref()
            .unwrap()
            .effective_query()
            .to_string()),
        "ab",
        "typing into the top dialog must not filter the one beneath"
    );

    // Settings' filter: escape leaves filter, a second escape closes Settings.
    vcx.simulate_keystrokes("escape escape");
    draw(&mut vcx);
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object]);
    assert!(shell.read_with(&vcx, |s, _| s.settings.is_none()));
    assert_eq!(input_text(&shell, &mut vcx), "ab");
    assert_eq!(input_cursor(&shell, &mut vcx), 2);
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.object_dialog.as_ref().unwrap().mode),
        DialogMode::Filter
    );
    assert!(dialog_filter_is_focused(&shell, &mut vcx));
}

/// Enter commits the top dialog (as-of: the highlighted preset) and pops
/// exactly one level.
#[gpui::test]
fn a_commit_pops_one_level(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "config::views");
    dispatch_action(&shell, "frame::as_of", &mut vcx);
    assert_eq!(
        kinds(&shell, &mut vcx),
        vec![DialogKind::Object, DialogKind::AsOf]
    );
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object]);
    assert!(shell.read_with(&vcx, |s, _| s.as_of_dialog.is_none()));
    assert!(shell.read_with(&vcx, |s, _| s.object_dialog.is_some()));
    assert!(
        vcx.debug_bounds("shell-modal-panel").is_some(),
        "Views paints again"
    );
}

/// A request for the kind already on top does nothing. A request for a kind lower
/// in the stack posts a notice and changes nothing: no push, no state overwrite.
#[gpui::test]
fn a_kind_already_in_the_stack_is_refused(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "config::views");
    vcx.simulate_keystrokes("/ a b");
    dispatch_action(&shell, "settings::open", &mut vcx);

    dispatch_action(&shell, "settings::open", &mut vcx);
    assert_eq!(
        kinds(&shell, &mut vcx),
        vec![DialogKind::Object, DialogKind::Settings]
    );
    assert_eq!(shell.read_with(&vcx, |s, _| s.notice), None);

    dispatch_action(&shell, "config::scopes", &mut vcx);
    assert_eq!(
        kinds(&shell, &mut vcx),
        vec![DialogKind::Object, DialogKind::Settings]
    );
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.notice),
        Some(DialogKind::Object.already_open_notice())
    );
    assert_eq!(
        shell.read_with(&vcx, |s, _| s
            .object_dialog
            .as_ref()
            .unwrap()
            .effective_query()
            .to_string()),
        "ab",
        "the refused request must not reinstall the live object dialog's state"
    );
}

/// The expression dialog's input text is its value. Covered by Settings, its typed
/// expression survives, and its suggestions are not recomputed from Settings' query.
/// Revealed, the text and caret come back and the field has focus.
#[gpui::test]
fn a_covered_expression_dialog_gets_its_typed_text_back(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "frame::scope_expression");
    vcx.simulate_input("book = ");
    let before = shell.read_with(&vcx, |s, _| {
        format!("{:?}", s.scope_expr_dialog.as_ref().unwrap().completion)
    });

    dispatch_action(&shell, "settings::open", &mut vcx);
    vcx.simulate_keystrokes("/ z z");
    assert_eq!(
        shell.read_with(&vcx, |s, _| format!(
            "{:?}",
            s.scope_expr_dialog.as_ref().unwrap().completion
        )),
        before,
        "a covered expression field must not refresh from another dialog's text"
    );

    vcx.simulate_keystrokes("escape escape");
    draw(&mut vcx);
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::ScopeExpr]);
    assert_eq!(input_text(&shell, &mut vcx), "book = ");
    assert_eq!(input_cursor(&shell, &mut vcx), "book = ".len());
    assert!(dialog_filter_is_focused(&shell, &mut vcx));
}

/// A covered picker still receives its distinct values, and shows them when revealed.
#[gpui::test]
fn a_covered_picker_receives_its_delivery(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, super::picker::services_with_pickable());
    let shell = shell_of(&window, &mut vcx);
    dispatch_action(&shell, "frame::pick_book", &mut vcx);
    let tag = shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().tag);
    dispatch_action(&shell, "frame::as_of", &mut vcx);
    assert_eq!(
        kinds(&shell, &mut vcx),
        vec![DialogKind::Picker, DialogKind::AsOf]
    );

    shell.update(&mut vcx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: PICKER_KEY,
                tag,
                column: "book".into(),
                values: Ok(vec![("BK000".into(), 3)]),
            },
            cx,
        )
    });
    assert!(shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().values.is_some()));

    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Picker]);
    assert!(vcx.debug_bounds("picker-value-BK000").is_some());
}
