//! Scope-expression integration: opening from the palette, committing through
//! `FrameViewMut::set_scope` for undo support, parse errors, empty-expression clearing, chip
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

/// `enter` on a typed expression commits it through `FrameViewMut::set_scope`,
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
            .shared()
            .scope()
            .expression
            .as_ref()
            .map(ToString::to_string)),
        Some("book = 'BK000'".to_string())
    );
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
    dispatch_action(&shell, "frame::scope_undo", &mut vcx);
    assert!(
        frame.read_with(&vcx, |f, _| f.shared().scope().expression.is_none()),
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
    assert!(frame.read_with(&vcx, |f, _| f.shared().scope().expression.is_none()));
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
        f.shared_mut().set_scope(expr_scope("book = 'BK000'"));
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
    assert!(frame.read_with(&vcx, |f, _| f.shared().scope().expression.is_none()));
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
        f.shared_mut().set_scope(expr_scope("book = 'BK000'"));
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
    services_with_schema_and(Vec::new())
}

/// [`services_with_schema`] plus two named expressions, `liq` and `hedges`.
fn services_with_named() -> ShellServices {
    services_with_schema_and(vec![
        LayerDoc::builtin(
            "expressions",
            "[liq]\nexpression = \"npv > 0\"\n[hedges]\nexpression = \"npv < 0\"\n",
        )
        .unwrap(),
    ])
}

/// [`services_with_schema`] with `extra` builtin layers after its own.
fn services_with_schema_and(extra: Vec<LayerDoc>) -> ShellServices {
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
    let mut builtin = vec![
        LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
        datasets,
        dims,
    ];
    builtin.extend(extra);
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin,
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
        f.shared_mut().set_scope(expr_scope("live = true"));
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

/// The platform undo key (`secondary-z`: cmd-z, or ctrl-z off macOS) takes
/// an insertion back, which proves the write kept undo.
#[gpui::test]
fn undo_takes_an_insertion_back(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    vcx.simulate_input("np");
    vcx.simulate_keystrokes("tab");
    vcx.run_until_parked();
    assert_eq!(field(&shell, &vcx), "npv ");
    vcx.simulate_keystrokes("secondary-z");
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

fn named_of(shell: &Entity<ShellView>, vcx: &gpui::VisualTestContext) -> Vec<String> {
    let frame = shell.read_with(vcx, |s, _| s.frame().clone());
    frame.read_with(vcx, |f, _| f.shared().scope().named.clone())
}

/// A shell over [`services_with_named`] whose frame scope names `named`
/// and holds `expression`.
fn named_shell(
    cx: &mut gpui::TestAppContext,
    named: &[&str],
    expression: Option<&str>,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    let (window, mut vcx) = open_shell(cx, services_with_named());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        f.shared_mut().set_scope(Scope {
            named: named.iter().map(ToString::to_string).collect(),
            expression: expression.map(|t| parse_expr(t).unwrap()),
            ..Scope::default()
        });
        cx.notify();
    });
    vcx.run_until_parked();
    (shell, vcx)
}

/// Tab on a named row erases the typed prefix and stages the name as a
/// chip; Enter on the then-empty field applies the name alone.
#[gpui::test]
fn tab_stages_a_named_row_and_enter_applies_it(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = named_shell(cx, &[], None);
    dispatch_action(&shell, "frame::add_expression", &mut vcx);
    vcx.simulate_input("liq");
    vcx.simulate_keystrokes("tab");
    vcx.run_until_parked();
    assert_eq!(field(&shell, &vcx), "", "the token is erased");
    assert!(
        vcx.debug_bounds("scope-expr-staged-liq").is_some(),
        "the staged chip paints"
    );
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
    assert_eq!(named_of(&shell, &vcx), vec!["liq".to_string()]);
}

/// Whole mode opens with the frame's names staged. Backspace at offset 0
/// removes the last chip, Enter applies names and text in one undoable
/// step, and one `frame::scope_undo` brings both names back.
#[gpui::test]
fn backspace_at_the_start_unstages_the_last_name_and_undo_restores(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = named_shell(cx, &["liq", "hedges"], Some("npv > 5"));
    dispatch_action(&shell, "frame::scope_expression", &mut vcx);
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-expr-staged-liq").is_some());
    assert!(vcx.debug_bounds("scope-expr-staged-hedges").is_some());
    vcx.simulate_keystrokes("home backspace");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-expr-staged-hedges").is_none());
    assert!(vcx.debug_bounds("scope-expr-staged-liq").is_some());
    assert_eq!(field(&shell, &vcx), "npv > 5", "the text is untouched");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(named_of(&shell, &vcx), vec!["liq".to_string()]);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert_eq!(
        frame.read_with(&vcx, |f, _| f
            .shared()
            .scope()
            .expression
            .as_ref()
            .map(ToString::to_string)),
        Some("npv > 5".to_string())
    );
    dispatch_action(&shell, "frame::scope_undo", &mut vcx);
    assert_eq!(
        named_of(&shell, &vcx),
        vec!["liq".to_string(), "hedges".to_string()],
        "one undo restores both names"
    );
}

/// Backspace away from offset 0 stays the field's: it deletes a
/// character and every staged chip stays.
#[gpui::test]
fn backspace_past_the_start_edits_the_text(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = named_shell(cx, &["liq"], Some("npv > 5"));
    dispatch_action(&shell, "frame::scope_expression", &mut vcx);
    vcx.simulate_keystrokes("backspace");
    vcx.run_until_parked();
    assert_eq!(field(&shell, &vcx), "npv > ");
    assert!(vcx.debug_bounds("scope-expr-staged-liq").is_some());
}

/// A staged name is no longer offered.
#[gpui::test]
fn a_staged_name_leaves_the_named_rows(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = named_shell(cx, &[], None);
    dispatch_action(&shell, "frame::add_expression", &mut vcx);
    vcx.run_until_parked();
    let row = vcx
        .debug_bounds("scope-expr-named-row-liq")
        .expect("liq is offered");
    vcx.simulate_click(row.center(), gpui::Modifiers::none());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-expr-staged-liq").is_some());
    assert!(
        vcx.debug_bounds("scope-expr-named-row-liq").is_none(),
        "staged, so not offered"
    );
    assert!(vcx.debug_bounds("scope-expr-named-row-hedges").is_some());
    assert_eq!(field(&shell, &vcx), "");
}

/// Term mode edits one term: it offers no named rows and stages nothing.
#[gpui::test]
fn term_mode_offers_no_named_rows(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = named_shell(cx, &["liq"], Some("npv > 5 and live = true"));
    let chip = vcx
        .debug_bounds("scope-expr-chip-0")
        .expect("the term chip paints");
    vcx.simulate_click(chip.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.scope_expr_dialog.as_ref().is_some_and(
            |d| matches!(d.mode, crate::shell::scope_expr_view::Mode::Term { .. })
        ))
    );
    vcx.simulate_keystrokes("secondary-a backspace");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("scope-expr-row-book").is_some(),
        "a column position"
    );
    assert!(vcx.debug_bounds("scope-expr-named-row-liq").is_none());
    assert!(vcx.debug_bounds("scope-expr-named-row-hedges").is_none());
    assert!(vcx.debug_bounds("scope-expr-staged-liq").is_none());
}

/// A click on a staged chip's `×` removes that name alone.
#[gpui::test]
fn clicking_a_staged_chips_close_unstages_it(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = named_shell(cx, &["liq", "hedges"], None);
    dispatch_action(&shell, "frame::scope_expression", &mut vcx);
    vcx.run_until_parked();
    let close = vcx
        .debug_bounds("scope-expr-staged-close-liq")
        .expect("the close paints");
    vcx.simulate_click(close.center(), gpui::Modifiers::none());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-expr-staged-liq").is_none());
    assert!(vcx.debug_bounds("scope-expr-staged-hedges").is_some());
    assert!(
        vcx.debug_bounds("scope-expr-named-row-liq").is_some(),
        "unstaged, so offered again"
    );
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(named_of(&shell, &vcx), vec!["hedges".to_string()]);
}

/// A named expression sharing a column's name: its named row stages it,
/// and the column row still inserts the column.
#[gpui::test]
fn a_named_row_and_a_column_row_of_one_name_click_apart(cx: &mut gpui::TestAppContext) {
    let services = services_with_schema_and(vec![
        LayerDoc::builtin("expressions", "[book]\nexpression = \"npv > 0\"\n").unwrap(),
    ]);
    let (shell, mut vcx) = dialog_test_shell_with(cx, services, "frame::add_expression");
    vcx.run_until_parked();
    let named = vcx
        .debug_bounds("scope-expr-named-row-book")
        .expect("the named row paints");
    vcx.simulate_click(named.center(), gpui::Modifiers::none());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-expr-staged-book").is_some());
    assert_eq!(field(&shell, &vcx), "", "a named row writes no text");
    let column = vcx
        .debug_bounds("scope-expr-row-book")
        .expect("the column row paints");
    vcx.simulate_click(column.center(), gpui::Modifiers::none());
    vcx.run_until_parked();
    assert_eq!(field(&shell, &vcx), "book ");
    assert!(vcx.debug_bounds("scope-expr-staged-book").is_some());
}

/// A reload that adds a named expression while the dialog is open offers
/// it straight away.
#[gpui::test]
fn a_reload_offers_a_new_named_expression_under_an_open_dialog(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_named(), "frame::add_expression");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-expr-named-row-liq").is_some());
    assert!(vcx.debug_bounds("scope-expr-named-row-fresh").is_none());
    let reloaded = services_with_schema_and(vec![
        LayerDoc::builtin(
            "expressions",
            "[liq]\nexpression = \"npv > 0\"\n[fresh]\nexpression = \"npv > 9\"\n",
        )
        .unwrap(),
    ]);
    shell.update(&mut vcx, |s, cx| s.apply_reload(reloaded.config, cx));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-expr-named-row-fresh").is_some());
    assert!(
        vcx.debug_bounds("scope-expr-named-row-hedges").is_none(),
        "a removed definition is no longer offered"
    );
}

fn expr_hint(shell: &Entity<ShellView>, vcx: &gpui::VisualTestContext) -> String {
    shell.read_with(vcx, |s, _| {
        s.scope_expr_dialog
            .as_ref()
            .map(|d| d.completion.hint().to_string())
            .unwrap_or_default()
    })
}

/// Staging a name changes the scope values are narrowed by, so a column
/// asked for before the staging is asked for again under the new names.
#[gpui::test]
fn staging_a_name_requests_values_again_under_it(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = named_shell(cx, &[], None);
    let seen = requests(&shell, &mut vcx);
    dispatch_action(&shell, "frame::add_expression", &mut vcx);
    vcx.simulate_input("book = ");
    vcx.run_until_parked();
    let first = seen.borrow().last().cloned().expect("a request");
    assert_eq!(first.scope.expression, None);
    vcx.simulate_keystrokes("secondary-a backspace");
    vcx.simulate_input("liq");
    vcx.simulate_keystrokes("tab");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-expr-staged-liq").is_some());
    vcx.simulate_input("book = ");
    vcx.run_until_parked();
    let second = seen.borrow().last().cloned().expect("a request");
    assert_ne!(second.tag, first.tag, "book is asked for again");
    assert_eq!(second.column, "book");
    // The request carries the resolved scope: `liq` is folded into the
    // expression, so its text is what narrows.
    assert!(second.scope.named.is_empty());
    assert_eq!(
        second.scope.expression.map(|e| e.to_string()),
        Some("npv > 0".to_string())
    );
}

/// A missing staged name is the value position's error; removing its
/// chip asks for the values again, and the error goes with it.
#[gpui::test]
fn unstaging_a_missing_name_clears_its_values_error(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = named_shell(cx, &["gone"], None);
    let seen = requests(&shell, &mut vcx);
    dispatch_action(&shell, "frame::scope_expression", &mut vcx);
    vcx.simulate_input("book = ");
    vcx.run_until_parked();
    assert!(seen.borrow().is_empty(), "a missing name never requests");
    assert!(
        expr_hint(&shell, &vcx).contains("gone"),
        "{}",
        expr_hint(&shell, &vcx)
    );
    let close = vcx
        .debug_bounds("scope-expr-staged-close-gone")
        .expect("the close paints");
    vcx.simulate_click(close.center(), gpui::Modifiers::none());
    vcx.run_until_parked();
    vcx.simulate_keystrokes("secondary-a backspace");
    vcx.simulate_input("book = ");
    vcx.run_until_parked();
    let req = seen.borrow().last().cloned().expect("asked again");
    assert_eq!(req.column, "book");
    assert!(req.scope.named.is_empty());
    assert!(
        !expr_hint(&shell, &vcx).contains("gone"),
        "{}",
        expr_hint(&shell, &vcx)
    );
}

/// Add mode joins the frame's names, so a name the frame already has is
/// not offered: staging it would change nothing.
#[gpui::test]
fn add_mode_leaves_out_the_frames_own_names(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = named_shell(cx, &["liq"], None);
    dispatch_action(&shell, "frame::add_expression", &mut vcx);
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-expr-named-row-liq").is_none());
    assert!(vcx.debug_bounds("scope-expr-named-row-hedges").is_some());
}

/// Let the zero-delay config write promote and reach the file.
fn flush_config_write(vcx: &mut gpui::VisualTestContext) {
    vcx.executor().advance_clock(
        crate::shell::objectdialog::apply::WRITE_DEBOUNCE + std::time::Duration::from_millis(10),
    );
    vcx.run_until_parked();
}

fn expr_error(shell: &Entity<ShellView>, vcx: &gpui::VisualTestContext) -> Option<String> {
    shell.read_with(vcx, |s, _| {
        s.scope_expr_dialog.as_ref().and_then(|d| d.error.clone())
    })
}

/// A frame dialog over [`services_with_named`] with a writable user dir.
fn saving_shell(
    cx: &mut gpui::TestAppContext,
    dir: &tempfile::TempDir,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    dialog_test_shell_in_dir(
        cx,
        services_with_named(),
        dir.path(),
        "frame::scope_expression",
    )
}

/// `mod+s` (the test config's alias is alt) names the typed text: Enter
/// writes `[liq2] expression = "npv > 0"` to the user file, empties the
/// field and stages the name. The frame knows the name at once, so the
/// Enter that applies it resolves before the write has even flushed.
#[gpui::test]
fn mod_s_saves_the_text_as_a_named_expression_and_stages_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = saving_shell(cx, &dir);
    vcx.simulate_input("npv > 0");
    vcx.simulate_keystrokes("alt-s");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("dialog-name-row").is_some(),
        "the name entry paints"
    );
    assert_eq!(field(&shell, &vcx), "", "the name entry starts empty");
    vcx.simulate_input("liq2");
    // The write promotes on a timer the test executor runs while parking,
    // which rebuilds the definitions anyway; the frame must already know
    // the name at the notify that stages it, before any timer runs.
    let known_when_staged = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    vcx.update(|_, cx| {
        let seen = known_when_staged.clone();
        let frame = shell.read(cx).frame().clone();
        cx.observe(&shell, move |shell, cx| {
            let staged = shell
                .read(cx)
                .scope_expr_dialog
                .as_ref()
                .is_some_and(|d| d.staged.iter().any(|s| s == "liq2"));
            if staged {
                let known = matches!(
                    frame.read(cx).named_expressions().get("liq2"),
                    Some(geode_core::named::NamedExpr::Valid { .. })
                );
                seen.borrow_mut().push(known);
            }
        })
        .detach();
    });
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(
        known_when_staged.borrow().first(),
        Some(&true),
        "the frame resolves the name the moment it is staged"
    );
    assert_eq!(expr_error(&shell, &vcx), None);
    assert!(vcx.debug_bounds("dialog-name-row").is_none());
    assert_eq!(field(&shell, &vcx), "", "the field is cleared");
    assert!(
        vcx.debug_bounds("scope-expr-staged-liq2").is_some(),
        "the saved name is staged"
    );
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
    assert_eq!(named_of(&shell, &vcx), vec!["liq2".to_string()]);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    let resolved = frame.read_with(&vcx, |f, _| f.shared().effective_scope(&Scope::default()));
    assert!(
        resolved.is_ok(),
        "the new name resolves before the write flushes: {resolved:?}"
    );
    flush_config_write(&mut vcx);
    let written = std::fs::read_to_string(dir.path().join("expressions.toml")).unwrap();
    assert!(
        written.contains("[liq2]\nexpression = \"npv > 0\""),
        "{written}"
    );
}

/// `mod+s` on an empty field refuses at once: the error line says the
/// expression is empty, no name entry opens, and nothing is written.
#[gpui::test]
fn mod_s_on_an_empty_field_refuses_and_writes_nothing(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = saving_shell(cx, &dir);
    vcx.simulate_input("   ");
    vcx.simulate_keystrokes("alt-s");
    vcx.run_until_parked();
    assert_eq!(
        expr_error(&shell, &vcx).as_deref(),
        Some("nothing to save — the expression is empty")
    );
    assert!(vcx.debug_bounds("scope-expr-error").is_some());
    assert!(
        vcx.debug_bounds("dialog-name-row").is_none(),
        "no name entry opens"
    );
    assert_eq!(field(&shell, &vcx), "   ", "the field is left alone");
    flush_config_write(&mut vcx);
    assert!(!dir.path().join("expressions.toml").exists());
}

/// A reserved name refuses inline, the entry stays open, nothing is written.
#[gpui::test]
fn mod_s_refuses_a_reserved_name(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = saving_shell(cx, &dir);
    let reserved = geode_core::scopes::RESERVED_NAMES[0];
    vcx.simulate_input("npv > 0");
    vcx.simulate_keystrokes("alt-s");
    vcx.simulate_input(reserved);
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(
        expr_error(&shell, &vcx),
        Some(format!("'{reserved}' is reserved"))
    );
    assert!(
        vcx.debug_bounds("dialog-name-row").is_some(),
        "naming stays open"
    );
    assert!(shell.read_with(&vcx, |s, _| {
        s.scope_expr_dialog
            .as_ref()
            .is_some_and(|d| d.staged.is_empty())
    }));
    flush_config_write(&mut vcx);
    assert!(!dir.path().join("expressions.toml").exists());
}

/// Without a writable user config directory a save refuses inline with
/// the object dialog's wording; nothing is staged and the frame's named
/// expressions are unchanged.
#[gpui::test]
fn mod_s_without_a_user_dir_refuses(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_named(), "frame::scope_expression");
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    let before = frame.read_with(&vcx, |f, _| f.named_expressions().clone());
    vcx.simulate_input("npv > 0");
    vcx.simulate_keystrokes("alt-s");
    vcx.simulate_input("liq2");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(
        expr_error(&shell, &vcx).as_deref(),
        Some("no writable user config directory — nothing was changed")
    );
    assert!(
        vcx.debug_bounds("dialog-name-row").is_some(),
        "naming stays open"
    );
    assert!(vcx.debug_bounds("scope-expr-staged-liq2").is_none());
    assert!(shell.read_with(&vcx, |s, _| {
        s.scope_expr_dialog
            .as_ref()
            .is_some_and(|d| d.staged.is_empty())
    }));
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.named_expressions().clone()),
        before
    );
}

/// The `mod+s` footer chip paints in every mode, and is gone while naming.
#[gpui::test]
fn the_save_chip_paints_only_where_saving_works(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = saving_shell(cx, &dir);
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("scope-expr-save-hint").is_some(),
        "whole mode"
    );
    vcx.simulate_input("npv > 0");
    vcx.simulate_keystrokes("alt-s");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("scope-expr-save-hint").is_none(),
        "not while naming"
    );
    vcx.simulate_keystrokes("escape escape");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
    dispatch_action(&shell, "frame::add_expression", &mut vcx);
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("scope-expr-save-hint").is_some(),
        "add mode"
    );

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        f.shared_mut()
            .set_scope(expr_scope("npv > 5 and live = true"));
        cx.notify();
    });
    vcx.run_until_parked();
    let chip = vcx
        .debug_bounds("scope-expr-chip-0")
        .expect("the term chip paints");
    vcx.simulate_click(chip.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.scope_expr_dialog.as_ref().is_some_and(
            |d| matches!(d.mode, crate::shell::scope_expr_view::Mode::Term { .. })
        ))
    );
    assert!(
        vcx.debug_bounds("scope-expr-save-hint").is_some(),
        "term mode names the term"
    );
}

/// A name already defined refuses inline and the entry stays open.
#[gpui::test]
fn mod_s_refuses_a_taken_name(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = saving_shell(cx, &dir);
    vcx.simulate_input("npv > 0");
    vcx.simulate_keystrokes("alt-s");
    vcx.simulate_input("liq");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(
        expr_error(&shell, &vcx).as_deref(),
        Some("'liq' already exists")
    );
    assert!(vcx.debug_bounds("dialog-name-row").is_some());
    assert!(vcx.debug_bounds("scope-expr-staged-liq").is_none());
    flush_config_write(&mut vcx);
    assert!(!dir.path().join("expressions.toml").exists());
}

/// Escape leaves the name entry and restores the text; the dialog stays.
#[gpui::test]
fn escape_while_naming_restores_the_text(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = saving_shell(cx, &dir);
    vcx.simulate_input("npv > 0");
    vcx.simulate_keystrokes("alt-s");
    vcx.simulate_input("half");
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.modal_open()), "stays open");
    assert!(vcx.debug_bounds("dialog-name-row").is_none());
    assert_eq!(field(&shell, &vcx), "npv > 0");
    assert!(
        dialog_filter_is_focused(&shell, &mut vcx),
        "the field keeps the keyboard"
    );
}

/// The name entry offers no suggestions: a name is not an expression.
#[gpui::test]
fn naming_offers_no_suggestions(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_shell, mut vcx) = saving_shell(cx, &dir);
    // Text at a column position: an empty field would refuse `mod+s`.
    vcx.simulate_input("npv > 0 and ");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-expr-row-book").is_some());
    vcx.simulate_keystrokes("alt-s");
    vcx.simulate_input("bo");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("dialog-name-row").is_some());
    assert!(
        vcx.debug_bounds("scope-expr-row-book").is_none(),
        "no column rows on a name"
    );
    assert!(vcx.debug_bounds("scope-expr-named-row-liq").is_none());
}

/// A [`saving_shell`] with the dialog closed and `text` as the frame's
/// expression, then its term chip `index` clicked open in Term mode.
fn term_saving_shell(
    cx: &mut gpui::TestAppContext,
    dir: &tempfile::TempDir,
    text: &str,
    index: usize,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    let (shell, mut vcx) = saving_shell(cx, dir);
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        f.shared_mut().set_scope(expr_scope(text));
        cx.notify();
    });
    vcx.run_until_parked();
    let chip = vcx
        .debug_bounds(format!("scope-expr-chip-{index}").leak())
        .expect("the term chip paints");
    vcx.simulate_click(chip.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.scope_expr_dialog.as_ref().is_some_and(
            |d| matches!(d.mode, crate::shell::scope_expr_view::Mode::Term { .. })
        ))
    );
    (shell, vcx)
}

fn term_texts(shell: &Entity<ShellView>, vcx: &gpui::VisualTestContext) -> Vec<String> {
    shell.read_with(vcx, |s, cx| {
        s.frame()
            .read(cx)
            .shared()
            .scope()
            .expression
            .as_ref()
            .map(|e| e.conjuncts().iter().map(|t| t.to_string()).collect())
            .unwrap_or_default()
    })
}

/// Naming an existing term: `mod+s` in Term mode asks for a name, and Enter
/// saves the field's text under it and swaps the term for the name in one
/// scope change. The dialog closes, the term's chip becomes a named chip,
/// and one undo puts the plain term back (the definition stays).
#[gpui::test]
fn mod_s_in_term_mode_names_the_term(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = term_saving_shell(cx, &dir, "npv > 5 and live = true", 0);
    vcx.simulate_keystrokes("alt-s");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("dialog-name-row").is_some(),
        "the name entry paints"
    );
    vcx.simulate_input("big");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(expr_error(&shell, &vcx), None);
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()), "closes");
    assert_eq!(named_of(&shell, &vcx), vec!["big".to_string()]);
    assert_eq!(term_texts(&shell, &vcx), vec!["live = true".to_string()]);
    assert!(vcx.debug_bounds("scope-expr-chip-1").is_none());
    assert!(
        vcx.debug_bounds("scope-named-chip-big").is_some(),
        "the term's chip is now a named chip"
    );
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert!(
        matches!(
            frame.read_with(&vcx, |f, _| f.named_expressions().get("big").cloned()),
            Some(geode_core::named::NamedExpr::Valid { .. })
        ),
        "the name resolves before the write flushes"
    );
    flush_config_write(&mut vcx);
    let written = std::fs::read_to_string(dir.path().join("expressions.toml")).unwrap();
    assert!(
        written.contains("[big]\nexpression = \"npv > 5\""),
        "{written}"
    );
    assert!(
        frame.update(&mut vcx, |f, _| f.shared_mut().undo_scope()),
        "one step"
    );
    assert_eq!(
        term_texts(&shell, &vcx),
        vec!["npv > 5".to_string(), "live = true".to_string()]
    );
    assert!(named_of(&shell, &vcx).is_empty());
}

/// The saved definition is the field's text when `mod+s` is pressed, so an
/// edit made in Term mode before naming is what the name stands for.
#[gpui::test]
fn naming_a_term_saves_the_edited_text(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = term_saving_shell(cx, &dir, "npv > 5 and live = true", 1);
    vcx.simulate_input(" or npv > 9");
    vcx.simulate_keystrokes("alt-s");
    vcx.simulate_input("mixed");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(expr_error(&shell, &vcx), None);
    assert_eq!(term_texts(&shell, &vcx), vec!["npv > 5".to_string()]);
    assert_eq!(named_of(&shell, &vcx), vec!["mixed".to_string()]);
    flush_config_write(&mut vcx);
    let written = std::fs::read_to_string(dir.path().join("expressions.toml")).unwrap();
    assert!(
        written.contains("[mixed]\nexpression = \"live = true or npv > 9\""),
        "{written}"
    );
}

/// A term that changed underneath the open dialog refuses the save before
/// anything is written: the frame keeps its scope, no definition is queued,
/// and the name entry stays open with the term-gone message.
#[gpui::test]
fn naming_a_changed_term_refuses_and_writes_nothing(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = term_saving_shell(cx, &dir, "npv > 5 and live = true", 0);
    vcx.simulate_keystrokes("alt-s");
    vcx.simulate_input("big");
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        f.shared_mut()
            .set_scope(expr_scope("npv > 7 and live = true"));
        cx.notify();
    });
    vcx.run_until_parked();
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(
        expr_error(&shell, &vcx).as_deref(),
        Some(crate::shell::scope_expr_view::TERM_GONE)
    );
    assert!(vcx.debug_bounds("dialog-name-row").is_some(), "stays open");
    assert!(named_of(&shell, &vcx).is_empty());
    assert_eq!(
        term_texts(&shell, &vcx),
        vec!["npv > 7".to_string(), "live = true".to_string()]
    );
    assert!(
        frame.read_with(&vcx, |f, _| f.named_expressions().get("big").is_none()),
        "no definition"
    );
    flush_config_write(&mut vcx);
    assert!(!dir.path().join("expressions.toml").exists());
}

/// Every expression field requests values under one pool key, and the pool keeps
/// only the newest request per key. A dialog pushed over this one may have
/// replaced its outstanding request, so the covered field's request never
/// replies. When the dialog is revealed, its field must ask again rather than
/// keep showing "loading values…".
#[gpui::test]
fn a_revealed_field_asks_again_for_the_values_a_covering_dialog_may_have_replaced(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    let seen = requests(&shell, &mut vcx);
    vcx.simulate_input("book = ");
    vcx.run_until_parked();
    assert_eq!(seen.borrow().len(), 1, "asked once");
    let first = seen.borrow()[0].tag;

    dispatch_action(&shell, "settings::open", &mut vcx);
    vcx.run_until_parked();
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();

    assert_eq!(field(&shell, &vcx), "book = ");
    let again = seen.borrow().last().cloned().expect("a request");
    assert_eq!(seen.borrow().len(), 2, "the revealed field asks again");
    assert_ne!(again.tag, first);
    assert_eq!((again.key, again.column.as_str()), (EXPR_KEY, "book"));
}
