//! The scope bar's expression terms and its add-a-filter menu: the `+`
//! opens a menu whose rows dispatch `frame::pick` and
//! `frame::add_expression`; each top-level `and` term is its own chip,
//! whose body edits that term alone and whose `×` drops it alone; and the
//! two palette actions `frame::add_expression`/`frame::clear_expression`.
//! Mouse-opened dialogs are checked by TYPING after the click (the
//! mouse-opened-dialog rule).

use super::*;
use geode_core::scope::{Scope, parse_expr};

fn expr_scope(text: &str) -> Scope {
    Scope {
        expression: Some(parse_expr(text).unwrap()),
        ..Scope::default()
    }
}

fn terms(frame: &Entity<crate::frame::Frame>, vcx: &gpui::VisualTestContext) -> Vec<String> {
    frame.read_with(vcx, |f, _| {
        f.scope()
            .expression
            .as_ref()
            .map(|e| e.conjuncts().iter().map(|t| t.to_string()).collect())
            .unwrap_or_default()
    })
}

fn modal_title(shell: &Entity<ShellView>, vcx: &gpui::VisualTestContext) -> Option<String> {
    shell.read_with(vcx, |s, _| s.top_modal().map(|m| m.title.to_string()))
}

fn dialog_text(shell: &Entity<ShellView>, vcx: &gpui::VisualTestContext) -> String {
    shell.read_with(vcx, |s, cx| s.dialog_input.read(cx).value().to_string())
}

/// A shell whose frame carries `expr`, painted once.
fn shell_with_expr(
    cx: &mut gpui::TestAppContext,
    expr: &str,
) -> (
    Entity<ShellView>,
    Entity<crate::frame::Frame>,
    gpui::VisualTestContext,
) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        f.set_scope(expr_scope(expr));
        cx.notify();
    });
    vcx.run_until_parked();
    (shell, frame, vcx)
}

fn click(vcx: &mut gpui::VisualTestContext, selector: &'static str) {
    let bounds = vcx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} paints"));
    vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
}

/// Click `+`, then "Expression…": the dialog opens in add mode, typing
/// after the click lands in its field, and `enter` joins the typed
/// expression to the current one with `and`.
#[gpui::test]
fn the_plus_menus_expression_row_appends_with_and(cx: &mut gpui::TestAppContext) {
    let (shell, frame, mut vcx) = shell_with_expr(cx, "book = 'BK000'");
    click(&mut vcx, "scope-pick-chip");
    assert!(
        vcx.debug_bounds("scope-add-menu").is_some(),
        "the menu paints"
    );
    assert!(
        vcx.debug_bounds("scope-add-menu-key-dimension").is_some(),
        "Dimension… shows frame::pick's live binding"
    );
    assert!(
        vcx.debug_bounds("scope-add-menu-key-expression").is_none(),
        "Expression… has no default chord, so no key"
    );
    click(&mut vcx, "scope-add-menu-row-expression");
    assert!(shell.read_with(&vcx, |s, _| s.add_filter_menu.is_none()));
    assert_eq!(
        modal_title(&shell, &vcx).as_deref(),
        Some("Add scope expression")
    );
    assert!(
        vcx.debug_bounds("scope-expr-note").is_some(),
        "the add note"
    );
    assert_eq!(dialog_text(&shell, &vcx), "", "add mode opens empty");
    vcx.simulate_input("lhu = 'L1'");
    vcx.run_until_parked();
    assert_eq!(
        dialog_text(&shell, &vcx),
        "lhu = 'L1'",
        "typing after the click reaches the field"
    );
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
    assert_eq!(terms(&frame, &vcx), vec!["book = 'BK000'", "lhu = 'L1'"]);
    assert!(
        vcx.debug_bounds("scope-expr-chip-1").is_some(),
        "the new term paints as its own chip"
    );
}

/// The menu's keys: `j` moves (and wraps with `k`), `enter` commits the
/// highlighted row, `escape` closes without opening anything.
#[gpui::test]
fn the_plus_menu_answers_j_k_enter_and_escape(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    click(&mut vcx, "scope-pick-chip");
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.add_filter_menu.is_none()));
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open() && s.picker.is_none()));

    click(&mut vcx, "scope-pick-chip");
    vcx.simulate_keystrokes("k");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s
            .add_filter_menu
            .as_ref()
            .map(|m| m.highlighted)),
        Some(1),
        "k from the first row wraps to the last"
    );
    vcx.simulate_keystrokes("j");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s
            .add_filter_menu
            .as_ref()
            .map(|m| m.highlighted)),
        Some(0)
    );
    vcx.simulate_keystrokes("j enter");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.add_filter_menu.is_none()));
    assert_eq!(
        modal_title(&shell, &vcx).as_deref(),
        Some("Add scope expression"),
        "enter on Expression… opens the add dialog"
    );
    vcx.simulate_input("a = 1");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert_eq!(terms(&frame, &vcx), vec!["a = 1"], "none before: it is set");

    click(&mut vcx, "scope-pick-chip");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.picker.is_some()),
        "enter on Dimension… opens the picker"
    );
}

/// A press outside the open menu closes it and reaches nothing beneath:
/// the grouping readout under the pointer does not open its picker.
#[gpui::test]
fn a_click_outside_the_menu_closes_it_and_goes_no_further(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    click(&mut vcx, "scope-pick-chip");
    assert!(shell.read_with(&vcx, |s, _| s.add_filter_menu.is_some()));
    click(&mut vcx, "scope-grouping");
    assert!(shell.read_with(&vcx, |s, _| s.add_filter_menu.is_none()));
    assert!(
        shell.read_with(&vcx, |s, _| s.choice_dialog.is_none()),
        "the closing press does not also open the grouping picker"
    );
    click(&mut vcx, "scope-pick-chip");
    click(&mut vcx, "scope-pick-chip");
    assert!(
        shell.read_with(&vcx, |s, _| s.add_filter_menu.is_none()),
        "a second press on the + closes the menu rather than reopening it"
    );
}

/// Each `and` term is its own chip; a click on a term's `×` drops that
/// term alone and does NOT open the dialog its body opens.
#[gpui::test]
fn a_terms_close_glyph_drops_only_that_term(cx: &mut gpui::TestAppContext) {
    let (shell, frame, mut vcx) = shell_with_expr(cx, "a = 1 and b = 2 and c = 3");
    for sel in [
        "scope-expr-chip-0",
        "scope-expr-chip-1",
        "scope-expr-chip-2",
    ] {
        assert!(vcx.debug_bounds(sel).is_some(), "{sel} paints");
    }
    click(&mut vcx, "scope-expr-chip-close-1");
    assert_eq!(terms(&frame, &vcx), vec!["a = 1", "c = 3"]);
    assert!(
        shell.read_with(&vcx, |s, _| !s.modal_open()
            && s.scope_expr_dialog.is_none()),
        "dropping a term must not also open the dialog its body opens"
    );
    assert!(vcx.debug_bounds("scope-expr-chip-2").is_none());
    dispatch_action(&shell, "frame::scope_undo", &mut vcx);
    assert_eq!(
        terms(&frame, &vcx),
        vec!["a = 1", "b = 2", "c = 3"],
        "the drop went through set_scope"
    );
}

/// A click on a term's body opens the dialog seeded with that term
/// alone; typing after the click lands, and `enter` replaces only it.
#[gpui::test]
fn a_terms_body_edits_that_term_alone(cx: &mut gpui::TestAppContext) {
    let (shell, frame, mut vcx) = shell_with_expr(cx, "a = 1 and b = 2 and c = 3");
    click(&mut vcx, "scope-expr-chip-1");
    assert_eq!(
        modal_title(&shell, &vcx).as_deref(),
        Some("Edit scope term")
    );
    assert_eq!(dialog_text(&shell, &vcx), "b = 2", "seeded with the term");
    assert!(
        vcx.debug_bounds("scope-expr-note").is_some(),
        "the term note"
    );
    vcx.simulate_input(" or x = 9");
    vcx.run_until_parked();
    assert_eq!(
        dialog_text(&shell, &vcx),
        "b = 2 or x = 9",
        "typing after the click reaches the field"
    );
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
    assert_eq!(
        terms(&frame, &vcx),
        vec!["a = 1", "(b = 2) or (x = 9)", "c = 3"]
    );
}

/// If the scope loses the term while its dialog is open, `enter` refuses
/// inline instead of editing whichever term now has that index.
#[gpui::test]
fn a_term_gone_at_commit_refuses_inline(cx: &mut gpui::TestAppContext) {
    let (shell, frame, mut vcx) = shell_with_expr(cx, "a = 1 and b = 2");
    click(&mut vcx, "scope-expr-chip-1");
    frame.update(&mut vcx, |f, cx| {
        f.set_scope(expr_scope("a = 1"));
        cx.notify();
    });
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.modal_open()), "stays open");
    assert!(vcx.debug_bounds("scope-expr-error").is_some());
    assert_eq!(terms(&frame, &vcx), vec!["a = 1"], "nothing edited");
}

/// The scope is replaced underneath with the SAME number of terms: index
/// 1 still exists but holds a different term, so both an edit and an
/// empty (removing) commit refuse inline rather than touch `y = 2`.
#[gpui::test]
fn a_term_replaced_underneath_refuses_edit_and_removal(cx: &mut gpui::TestAppContext) {
    let (shell, frame, mut vcx) = shell_with_expr(cx, "a = 1 and b = 2");
    click(&mut vcx, "scope-expr-chip-1");
    frame.update(&mut vcx, |f, cx| {
        f.set_scope(expr_scope("x = 1 and y = 2"));
        cx.notify();
    });
    vcx.simulate_input(" or z = 3");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.modal_open()), "stays open");
    assert!(vcx.debug_bounds("scope-expr-error").is_some());
    assert_eq!(
        terms(&frame, &vcx),
        vec!["x = 1", "y = 2"],
        "nothing edited"
    );

    vcx.update(|window, cx| {
        let input = shell.read(cx).dialog_input.clone();
        input.update(cx, |i, cx| i.set_value("", window, cx));
    });
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.modal_open()), "stays open");
    assert_eq!(
        terms(&frame, &vcx),
        vec!["x = 1", "y = 2"],
        "an empty commit does not remove the other term"
    );
}

/// Focus the scope text field, then open the `+` menu.
fn open_menu_from_the_field(shell: &Entity<ShellView>, vcx: &mut gpui::VisualTestContext) {
    dispatch_action(shell, "frame::focus_text", vcx);
    assert!(filter_is_focused(shell, vcx));
    click(vcx, "scope-pick-chip");
    assert!(shell.read_with(vcx, |s, _| s.add_filter_menu.is_some()));
}

/// Opened while the scope text field held focus, the menu hands focus
/// back to the field when it closes itself (`escape`, an outside press)
/// and when the dialog one of its rows opened closes.
#[gpui::test]
fn focus_returns_to_the_text_field_after_the_menu(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);

    open_menu_from_the_field(&shell, &mut vcx);
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.add_filter_menu.is_none()));
    assert!(
        filter_is_focused(&shell, &mut vcx),
        "escape: back to the field"
    );

    open_menu_from_the_field(&shell, &mut vcx);
    click(&mut vcx, "scope-grouping");
    assert!(shell.read_with(&vcx, |s, _| s.add_filter_menu.is_none()));
    assert!(
        filter_is_focused(&shell, &mut vcx),
        "outside press: back to the field"
    );

    open_menu_from_the_field(&shell, &mut vcx);
    click(&mut vcx, "scope-add-menu-row-dimension");
    assert!(shell.read_with(&vcx, |s, _| s.picker.is_some()));
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
    assert!(
        filter_is_focused(&shell, &mut vcx),
        "the picker a row opened returns to the field"
    );

    open_menu_from_the_field(&shell, &mut vcx);
    vcx.simulate_keystrokes("j enter");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.scope_expr_dialog.is_some()));
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(
        filter_is_focused(&shell, &mut vcx),
        "the add dialog a row opened returns to the field"
    );
}

/// A right or middle press outside the menu closes it too (the catcher
/// swallows every button, so it must answer every button).
#[gpui::test]
fn any_button_outside_the_menu_closes_it(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    for button in [gpui::MouseButton::Right, gpui::MouseButton::Middle] {
        click(&mut vcx, "scope-pick-chip");
        assert!(shell.read_with(&vcx, |s, _| s.add_filter_menu.is_some()));
        let at = vcx
            .debug_bounds("scope-grouping")
            .expect("readout paints")
            .center();
        vcx.update(|window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::MouseDown(MouseDownEvent {
                    button,
                    position: at,
                    modifiers: gpui::Modifiers::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
        });
        vcx.run_until_parked();
        assert!(
            shell.read_with(&vcx, |s, _| s.add_filter_menu.is_none()),
            "{button:?} closes the menu"
        );
    }
}

/// A half-typed chord sequence does not outlive the menu: `escape`
/// cancels the pending prefix, so its second chord does nothing.
#[gpui::test]
fn closing_the_menu_cancels_a_pending_sequence(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    let user_doc = LayerDoc {
        layer: geode_core::config::Layer::User,
        name: "keymap".to_string(),
        file: "<test:user>".into(),
        table: "[[bindings]]\n[bindings.keys]\n\"ctrl+alt+g ctrl+alt+j\" = \"frame::clear_expression\"\n"
            .parse()
            .unwrap(),
    };
    services.keymap = test_keymap(&services.registry, &[user_doc]);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        f.set_scope(expr_scope("a = 1"));
        cx.notify();
    });
    vcx.run_until_parked();

    // Control: without the menu the sequence clears the expression.
    vcx.simulate_keystrokes("ctrl-alt-g ctrl-alt-j");
    vcx.run_until_parked();
    assert_eq!(
        terms(&frame, &vcx),
        Vec::<String>::new(),
        "the sequence works"
    );
    dispatch_action(&shell, "frame::scope_undo", &mut vcx);
    assert_eq!(terms(&frame, &vcx), vec!["a = 1"]);

    click(&mut vcx, "scope-pick-chip");
    vcx.simulate_keystrokes("ctrl-alt-g escape");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.add_filter_menu.is_none()));
    vcx.simulate_keystrokes("ctrl-alt-j");
    vcx.run_until_parked();
    assert_eq!(
        terms(&frame, &vcx),
        vec!["a = 1"],
        "the prefix typed while the menu was open was cancelled with it"
    );
}

/// The palette toggle closes the menu as it opens the palette.
#[gpui::test]
fn the_palette_toggle_closes_the_menu(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    click(&mut vcx, "scope-pick-chip");
    vcx.simulate_keystrokes("ctrl-k");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.palette.is_some()));
    assert!(shell.read_with(&vcx, |s, _| s.add_filter_menu.is_none()));
}

/// The `+` holds its pressed fill (and says so through its open marker)
/// for exactly as long as the menu is up.
#[gpui::test]
fn the_plus_holds_its_pressed_fill_while_the_menu_is_open(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let _shell = shell_of(&window, &mut vcx);
    assert!(vcx.debug_bounds("scope-pick-chip-open").is_none());
    click(&mut vcx, "scope-pick-chip");
    assert!(
        vcx.debug_bounds("scope-pick-chip-open").is_some(),
        "pressed while open"
    );
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-pick-chip-open").is_none());
}

/// The palette actions: `frame::add_expression` opens the add dialog
/// (empty, joined with `and`), and `frame::clear_expression` drops the
/// whole layer undoably.
#[gpui::test]
fn the_add_and_clear_expression_actions(cx: &mut gpui::TestAppContext) {
    let (shell, frame, mut vcx) = shell_with_expr(cx, "a = 1");
    dispatch_action(&shell, "frame::add_expression", &mut vcx);
    assert_eq!(
        modal_title(&shell, &vcx).as_deref(),
        Some("Add scope expression")
    );
    assert!(dialog_filter_is_focused(&shell, &mut vcx));
    assert_eq!(dialog_text(&shell, &vcx), "");
    vcx.simulate_input("b = 2");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(terms(&frame, &vcx), vec!["a = 1", "b = 2"]);

    dispatch_action(&shell, "frame::clear_expression", &mut vcx);
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.scope().expression.clone()),
        None
    );
    dispatch_action(&shell, "frame::scope_undo", &mut vcx);
    assert_eq!(terms(&frame, &vcx), vec!["a = 1", "b = 2"]);

    dispatch_action(&shell, "frame::scope_expression", &mut vcx);
    assert_eq!(
        modal_title(&shell, &vcx).as_deref(),
        Some("Scope expression")
    );
    assert_eq!(
        dialog_text(&shell, &vcx),
        "(a = 1) and (b = 2)",
        "scope_expression keeps whole mode"
    );
}
