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

/// A `ScopeExpr` dialog pushed over the Scopes object dialog's open
/// `expression` field: accepting a suggestion in the top field must write
/// only the top field's own completion, never the covered field's draft.
/// Popping reveals the covered field exactly as it was left.
#[gpui::test]
fn accept_in_a_pushed_dialog_does_not_touch_a_covered_object_draft(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = dialog_test_shell_in_dir(
        cx,
        super::objectdialog::services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    super::objectdialog::open_expression_field(&shell, &mut vcx);
    vcx.simulate_input("boo");
    vcx.run_until_parked();
    assert_eq!(
        super::objectdialog::edit_draft(&shell, &vcx, |d| d.query.clone()),
        "boo"
    );

    dispatch_action(&shell, "frame::scope_expression", &mut vcx);
    assert_eq!(
        kinds(&shell, &mut vcx),
        vec![DialogKind::Object, DialogKind::ScopeExpr]
    );

    vcx.simulate_input("np");
    vcx.simulate_keystrokes("tab");
    vcx.run_until_parked();
    assert_eq!(
        input_text(&shell, &mut vcx),
        "npv ",
        "the top field accepted its own suggestion"
    );
    assert_eq!(
        super::objectdialog::edit_draft(&shell, &vcx, |d| d.query.clone()),
        "boo",
        "an accept in the top dialog must not overwrite the covered object draft"
    );

    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object]);
    assert_eq!(
        input_text(&shell, &mut vcx),
        "boo",
        "the revealed field has its typed text back, not the top field's accepted text"
    );
}

/// A dialog-opening chord pushes over an open dialog: `mod+t` (alt under the test
/// mod alias) opens as-of, and `ctrl+,` opens Settings.
#[gpui::test]
fn a_dialog_chord_pushes_over_an_open_dialog(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "config::views");
    vcx.simulate_keystrokes("alt-t");
    assert_eq!(
        kinds(&shell, &mut vcx),
        vec![DialogKind::Object, DialogKind::AsOf]
    );
    vcx.simulate_keystrokes("ctrl-,");
    assert_eq!(
        kinds(&shell, &mut vcx),
        vec![DialogKind::Object, DialogKind::AsOf, DialogKind::Settings]
    );
}

/// Any other chord stays inert behind a dialog: `ctrl+=` must not grow the font.
#[gpui::test]
fn a_non_dialog_chord_is_inert_behind_a_dialog(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "config::views");
    let before = shell.read_with(&vcx, |s, _| s.font_size);
    vcx.simulate_keystrokes("ctrl-=");
    assert_eq!(shell.read_with(&vcx, |s, _| s.font_size), before);
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object]);
}

/// `opens_dialog` matches dispatch in both directions. Over a stateless base modal,
/// every registered action is dispatched: a flagged action must push, and an
/// unflagged one must not.
#[gpui::test]
fn opens_dialog_matches_what_dispatch_pushes(cx: &mut gpui::TestAppContext) {
    // Flagged actions that legitimately refuse in this fixture, each with the reason.
    const REFUSES_IN_FIXTURE: &[&str] = &[];
    let (window, mut vcx) = open_shell(cx, super::picker::services_with_pickable());
    let shell = shell_of(&window, &mut vcx);
    let ids: Vec<crate::actions::ActionId> = shell.read_with(&vcx, |s, _| {
        s.services.registry.iter().map(|d| d.id.clone()).collect()
    });
    let mut wrong = Vec::new();
    for id in ids {
        vcx.update(|window, cx| {
            shell.update(cx, |s, cx| {
                s.close_palette(window, cx);
                s.cancel_command_line(window, cx);
                while s.modal_open() {
                    s.close_modal(window, cx);
                }
                crate::shell::dialog::open_shell_dialog(
                    s,
                    window,
                    cx,
                    DialogKind::Plain,
                    "Base",
                    |_, _, _| gpui::div().into_any_element(),
                );
                s.dispatch(&id, None, window, cx);
            });
        });
        let pushed = shell.read_with(&vcx, |s, _| s.modal_depth() > 1);
        let flagged = crate::shell::dialog::opens_dialog(&id);
        if pushed != flagged && !(flagged && REFUSES_IN_FIXTURE.contains(&id.0.as_str())) {
            wrong.push(format!("{} pushed={pushed} flagged={flagged}", id.0));
        }
    }
    assert!(
        wrong.is_empty(),
        "opens_dialog disagrees with dispatch: {wrong:#?}"
    );
}

/// A dialog-opening chord pushes over Settings in Normal mode too: the
/// Object dialog is not the only Normal-mode catch-all that must decline an
/// unrecognized chord.
#[gpui::test]
fn a_dialog_chord_pushes_over_settings_in_normal_mode(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "settings::open");
    vcx.simulate_keystrokes("alt-t");
    assert_eq!(
        kinds(&shell, &mut vcx),
        vec![DialogKind::Settings, DialogKind::AsOf]
    );
}

/// Keybindings capture claims every key, chords included, so recording a
/// dialog-opening chord captures it as the new binding rather than
/// dispatching it and pushing another dialog over this one.
#[gpui::test]
fn capture_records_a_dialog_opening_chord_instead_of_pushing_over_it(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, mut vcx) = dialog_test_shell(cx, "keybindings::open");
    vcx.simulate_keystrokes("enter");
    assert!(
        shell.read_with(&vcx, |s, _| s
            .keybindings
            .as_ref()
            .unwrap()
            .listening
            .is_some()),
        "enter should start listening on the selected row"
    );

    vcx.simulate_keystrokes("ctrl-,");
    assert_eq!(
        kinds(&shell, &mut vcx),
        vec![DialogKind::Keybindings],
        "ctrl-, must be captured, not dispatched as settings::open"
    );
    let pending = shell.read_with(&vcx, |s, _| {
        s.keybindings.as_ref().unwrap().listening.clone()
    });
    assert_eq!(
        pending.as_deref().map(<[_]>::len),
        Some(1),
        "the chord must land in the pending binding"
    );
}

/// The palette opens above a dialog and paints above it. Escape closes only the
/// palette, and the dialog has its focus back.
#[gpui::test]
fn the_palette_opens_over_a_dialog_and_escape_returns_to_it(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "config::views");
    vcx.simulate_keystrokes("/ a b");
    vcx.simulate_keystrokes("ctrl-k");
    draw(&mut vcx);
    assert!(shell.read_with(&vcx, |s, _| s.palette.is_some()));
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object]);
    assert!(vcx.debug_bounds("palette-click-catcher").is_some());

    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);
    assert!(shell.read_with(&vcx, |s, _| s.palette.is_none()));
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object]);
    assert_eq!(input_text(&shell, &mut vcx), "ab");
    assert!(dialog_filter_is_focused(&shell, &mut vcx));
}

/// A dialog-opening palette entry pushes its dialog over the stack.
#[gpui::test]
fn a_palette_dialog_entry_pushes(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "config::views");
    vcx.simulate_keystrokes("ctrl-k");
    vcx.simulate_input("Open settings");
    let selected = shell.read_with(&vcx, |s, _| s.palette.as_ref().unwrap().selected_item());
    assert!(
        matches!(&selected, Some(crate::palette::PaletteItem::Action(id, ..)) if id.0 == "settings::open"),
        "{selected:?}"
    );
    vcx.simulate_keystrokes("enter");
    assert_eq!(
        kinds(&shell, &mut vcx),
        vec![DialogKind::Object, DialogKind::Settings]
    );
}

/// A non-dialog palette action runs behind the stack. The stack stays, and the
/// top dialog keeps focus even though the action armed a tile focus restore.
#[gpui::test]
fn a_palette_action_behind_the_stack_leaves_focus_on_the_top_dialog(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "config::views");
    vcx.simulate_keystrokes("/ a b");
    let before = shell.read_with(&vcx, |s, _| s.line_numbers);
    vcx.simulate_keystrokes("ctrl-k");
    vcx.simulate_input("line numbers");
    let selected = shell.read_with(&vcx, |s, _| s.palette.as_ref().unwrap().selected_item());
    assert!(
        matches!(&selected, Some(crate::palette::PaletteItem::Action(id, ..)) if id.0 == "ui::line_numbers_cycle"),
        "{selected:?}"
    );
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert_ne!(
        shell.read_with(&vcx, |s, _| s.line_numbers),
        before,
        "the action ran"
    );
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object]);
    assert!(dialog_filter_is_focused(&shell, &mut vcx));

    // A workspace action runs through the tile focus reconciliation behind the
    // stack; the renders after it leave focus on the top dialog. (The palette
    // closes and refocuses the dialog before dispatch, so this route cannot arm
    // `pending_focus_restore`; the reload test below covers that flag.)
    vcx.simulate_keystrokes("ctrl-k");
    vcx.simulate_input("Focus left");
    let selected = shell.read_with(&vcx, |s, _| s.palette.as_ref().unwrap().selected_item());
    assert!(
        matches!(&selected, Some(crate::palette::PaletteItem::Action(id, ..)) if id.0 == "workspace::focus_left"),
        "{selected:?}"
    );
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    draw(&mut vcx);
    assert!(dialog_filter_is_focused(&shell, &mut vcx));
}

/// The first dialog was opened from the scope-bar field. A pushed dialog and a
/// palette opened and closed mid-stack must not overwrite that. The last pop
/// returns focus to the field.
#[gpui::test]
fn the_last_pop_returns_to_the_field_after_a_palette_mid_stack(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    dispatch_action(&shell, "frame::focus_text", &mut vcx);
    assert!(filter_is_focused(&shell, &mut vcx));
    vcx.simulate_keystrokes("alt-t");
    vcx.simulate_keystrokes("ctrl-,");
    vcx.simulate_keystrokes("ctrl-k");
    vcx.simulate_keystrokes("escape");
    assert_eq!(
        kinds(&shell, &mut vcx),
        vec![DialogKind::AsOf, DialogKind::Settings]
    );
    vcx.simulate_keystrokes("escape");
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::AsOf]);
    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);
    assert!(!shell.read_with(&vcx, |s, _| s.modal_open()));
    assert!(filter_is_focused(&shell, &mut vcx));
}

/// A reload that closes a palette opened over the stack has no `Window`, so it
/// arms `pending_focus_restore`. The next render hands focus back to the top
/// dialog, not to the shell root, and not left on the dropped palette input.
#[gpui::test]
fn a_reload_closing_a_palette_over_the_stack_refocuses_the_top_dialog(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, mut vcx) = dialog_test_shell(cx, "config::views");
    vcx.simulate_keystrokes("/ a b");
    vcx.simulate_keystrokes("ctrl-k");
    assert!(shell.read_with(&vcx, |s, _| s.palette.is_some()));

    shell.update(&mut vcx, |s, cx| s.apply_reload(config_with_mod("cmd"), cx));
    assert!(shell.read_with(&vcx, |s, _| s.palette.is_none()));
    draw(&mut vcx);
    draw(&mut vcx);
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object]);
    assert_eq!(input_text(&shell, &mut vcx), "ab");
    assert!(dialog_filter_is_focused(&shell, &mut vcx));
}

/// A palette action behind the stack that moves focus itself (to the scope-bar
/// field) still leaves the keyboard on the top dialog once the palette closes.
#[gpui::test]
fn a_focus_moving_palette_action_behind_the_stack_is_refocused_to_the_dialog(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, mut vcx) = dialog_test_shell(cx, "config::views");
    vcx.simulate_keystrokes("/ a b");
    vcx.simulate_keystrokes("ctrl-k");
    vcx.simulate_input("Focus the scope text");
    let selected = shell.read_with(&vcx, |s, _| s.palette.as_ref().unwrap().selected_item());
    assert!(
        matches!(&selected, Some(crate::palette::PaletteItem::Action(id, ..)) if id.0 == "frame::focus_text"),
        "{selected:?}"
    );
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object]);
    assert!(!filter_is_focused(&shell, &mut vcx));
    assert!(dialog_filter_is_focused(&shell, &mut vcx));
}
