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
                assert_eq!(
                    s.modal_depth(),
                    1,
                    "the Plain base itself must land on the stack, or every \
                     `pushed` reading below is measuring against nothing"
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

/// The object dialog's edit stage is a second Normal-mode catch-all beside
/// browse: `a_dialog_chord_pushes_over_an_open_dialog` only reaches the
/// browse-stage decline, so this pushes over `mine`'s open edit stage.
#[gpui::test]
fn a_dialog_chord_pushes_over_the_object_edit_stage(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell_with(
        cx,
        super::objectdialog::services_with_a_saved_scope(),
        "config::scopes",
    );
    vcx.simulate_keystrokes("enter"); // open `mine`'s edit stage
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.object_dialog.as_ref().unwrap().stage.clone()),
        crate::shell::objectdialog::Stage::Edit {
            object: "mine".to_string()
        }
    );
    vcx.simulate_keystrokes("alt-t");
    assert_eq!(
        kinds(&shell, &mut vcx),
        vec![DialogKind::Object, DialogKind::AsOf]
    );
}

/// Keybindings in Normal mode (not capturing a keystroke) is a fourth
/// Normal-mode catch-all: `capture_records_a_dialog_opening_chord_instead_of_pushing_over_it`
/// covers capture, which claims every key including chords, so this covers
/// the decline outside capture.
#[gpui::test]
fn a_dialog_chord_pushes_over_keybindings_in_normal_mode(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "keybindings::open");
    vcx.simulate_keystrokes("alt-t");
    assert_eq!(
        kinds(&shell, &mut vcx),
        vec![DialogKind::Keybindings, DialogKind::AsOf]
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
/// top dialog keeps focus because closing the palette already restored it
/// before dispatch runs, not because the action itself arms a focus restore.
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

/// The click-catcher behind the palette panel has no `occlude()`, so
/// `on_mouse_down` (which gpui fires for every hovered hitbox, not just the
/// topmost) reaches `shell-modal-backdrop` beneath it as well: a click
/// outside both panels closes the palette AND pops the dialog underneath.
/// It must close only the palette.
#[gpui::test]
fn a_click_outside_the_palette_over_a_dialog_closes_only_the_palette(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, mut vcx) = dialog_test_shell(cx, "config::views");
    vcx.simulate_keystrokes("ctrl-k");
    draw(&mut vcx);
    assert!(shell.read_with(&vcx, |s, _| s.palette.is_some()));
    assert!(vcx.debug_bounds("palette-click-catcher").is_some());

    // Top-left corner of the viewport: both the modal panel and the
    // palette panel are centered with a top margin, so this point is
    // outside both, but still inside the full-viewport catcher.
    let outside = gpui::point(px(2.), px(2.));
    vcx.simulate_click(outside, gpui::Modifiers::default());
    draw(&mut vcx);

    assert!(
        shell.read_with(&vcx, |s, _| s.palette.is_none()),
        "the click outside the palette must close it"
    );
    assert_eq!(
        kinds(&shell, &mut vcx),
        vec![DialogKind::Object],
        "the click must not also reach shell-modal-backdrop and pop the dialog beneath"
    );
}

/// A click at a dialog row's position is also a click on the catcher (it
/// covers the full viewport, including wherever the dialog's own rows paint
/// underneath). Without `occlude()`, the same click also reaches the row's
/// own handler and opens it.
#[gpui::test]
fn a_click_on_a_dialog_row_under_the_palette_does_not_open_it(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell_with(
        cx,
        super::objectdialog::services_with_a_saved_scope(),
        "config::scopes",
    );
    let row = vcx
        .debug_bounds("objectdialog-row-mine")
        .expect("the mine row paints");
    vcx.simulate_keystrokes("ctrl-k");
    draw(&mut vcx);
    assert!(shell.read_with(&vcx, |s, _| s.palette.is_some()));
    let palette_panel = vcx
        .debug_bounds("palette-panel")
        .expect("the palette panel paints");

    // A point in the row's own band but to the left of the (narrower) palette
    // panel: not a click on the palette's stop-propagation panel, so it is
    // the catcher, not the panel, that must keep it from also reaching the
    // row underneath.
    let outside_the_panel = gpui::point(row.origin.x + px(4.), row.origin.y + row.size.height / 2.);
    assert!(
        outside_the_panel.x < palette_panel.origin.x,
        "the probe point must fall outside the palette panel: {outside_the_panel:?} vs {palette_panel:?}"
    );
    vcx.simulate_click(outside_the_panel, gpui::Modifiers::default());
    draw(&mut vcx);

    assert!(
        shell.read_with(&vcx, |s, _| s.palette.is_none()),
        "the click closed the palette"
    );
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.object_dialog.as_ref().unwrap().stage.clone()),
        crate::shell::objectdialog::Stage::Browse,
        "the click must not also reach the row and open it"
    );
}

/// Reverses the earlier "known limitation": `tile::command_line`, `tile::find`,
/// and `stack::pick`, run from the palette over a dialog, used to open real
/// transient chrome behind the stack that the user could not see or usefully
/// reach. Each must now refuse with a notice and leave the stack untouched.
#[gpui::test]
fn palette_transient_chrome_is_refused_over_a_dialog(cx: &mut gpui::TestAppContext) {
    let (mut vcx, shell, _log, _left, _right, _top) = super::stacks::stacked_shell(cx);
    for (query, id) in [
        ("Open the tile command line", "tile::command_line"),
        ("Find in tile", "tile::find"),
        ("Stack: Pick", "stack::pick"),
    ] {
        dispatch_action(&shell, "config::views", &mut vcx);
        draw(&mut vcx);
        assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object]);

        vcx.simulate_keystrokes("ctrl-k");
        vcx.simulate_input(query);
        let selected = shell.read_with(&vcx, |s, _| s.palette.as_ref().unwrap().selected_item());
        assert!(
            matches!(&selected, Some(crate::palette::PaletteItem::Action(a, ..)) if a.0 == id),
            "{id}: {selected:?}"
        );
        vcx.simulate_keystrokes("enter");
        draw(&mut vcx);

        assert!(
            shell.read_with(&vcx, |s, _| s.notice.is_some()),
            "{id} must set a notice"
        );
        assert!(
            shell.read_with(&vcx, |s, _| s.command_line.is_none()),
            "{id} must not open the command line"
        );
        assert!(
            shell.read_with(&vcx, |s, _| s.stack_list.is_none()),
            "{id} must not open the stack list"
        );
        assert_eq!(
            kinds(&shell, &mut vcx),
            vec![DialogKind::Object],
            "{id} must not change the stack"
        );

        vcx.update(|window, cx| {
            shell.update(cx, |s, cx| {
                while s.modal_open() {
                    s.close_modal(window, cx);
                }
            });
        });
    }
}

/// Regression guard for a reviewed finding that turned out not to reproduce:
/// `dispatch_palette_item`'s `is_toggle` guard already skips dispatch for the
/// palette's own "Toggle command palette" row, so picking it over a dialog
/// just closes the palette — it does not re-dispatch `palette::toggle` and
/// reopen it, so `commit_selected`'s post-dispatch `refocus_top` has nothing
/// to steal focus from.
#[gpui::test]
fn picking_the_toggle_row_over_a_dialog_does_not_reopen_the_palette(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "config::views");
    vcx.simulate_keystrokes("/ a b");
    vcx.simulate_keystrokes("ctrl-k");
    vcx.simulate_input("Toggle command palette");
    let selected = shell.read_with(&vcx, |s, _| s.palette.as_ref().unwrap().selected_item());
    assert!(
        matches!(&selected, Some(crate::palette::PaletteItem::Action(id, ..)) if id.0 == "palette::toggle"),
        "{selected:?}"
    );
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert!(
        shell.read_with(&vcx, |s, _| s.palette.is_none()),
        "the palette must not reopen"
    );
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::Object]);
    assert_eq!(input_text(&shell, &mut vcx), "ab");
    assert!(dialog_filter_is_focused(&shell, &mut vcx));
}

/// `test_services` with a `datasets` doc naming one column (`book`), for a
/// reload test that adds a second column while a `ScopeExpr` dialog
/// referencing it is covered by another dialog.
fn services_with_one_dataset_column() -> ShellServices {
    let mut services = test_services();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
    )
    .unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            datasets,
        ],
        desk: None,
        user: None,
    });
    services
}

/// `services_with_one_dataset_column`'s `datasets` doc with `zzcol` added,
/// for `apply_reload` to pick up.
fn config_with_a_second_dataset_column() -> Config {
    Config::load(&ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            LayerDoc::builtin(
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
                 [risk.columns.zzcol]\ntype = \"utf8\"\nrole = \"attribute\"\ngrain = \"position\"\n",
            )
            .unwrap(),
        ],
        desk: None,
        user: None,
    })
}

/// A hot reload that adds a dataset column must refresh a COVERED
/// `ScopeExpr` dialog's suggestions too, not only the top dialog's:
/// `hot_reload`'s `pickable_changed` branch used to rebuild only
/// `top_kind()`'s completion, so a covered expression field kept its stale
/// "unknown column" warning until its own next keystroke.
#[gpui::test]
fn a_reload_refreshes_a_covered_expression_dialogs_suggestions(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell_with(
        cx,
        services_with_one_dataset_column(),
        "frame::scope_expression",
    );
    vcx.simulate_input("zzcol = 'x'");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s
            .scope_expr_dialog
            .as_ref()
            .unwrap()
            .completion
            .warning()
            .is_some()),
        "zzcol is not yet a known column, so the field should warn"
    );

    dispatch_action(&shell, "settings::open", &mut vcx);
    assert_eq!(
        kinds(&shell, &mut vcx),
        vec![DialogKind::ScopeExpr, DialogKind::Settings]
    );

    shell.update(&mut vcx, |s, cx| {
        s.apply_reload(config_with_a_second_dataset_column(), cx)
    });
    // Still covered by Settings: the fix rebuilds it at reload time, not at
    // reveal, so the warning must already be gone here.
    assert!(
        shell.read_with(&vcx, |s, _| s
            .scope_expr_dialog
            .as_ref()
            .unwrap()
            .completion
            .warning()
            .is_none()),
        "a covered dialog's suggestions must refresh at reload, while still covered"
    );

    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);
    assert_eq!(kinds(&shell, &mut vcx), vec![DialogKind::ScopeExpr]);
    assert!(
        shell.read_with(&vcx, |s, _| s
            .scope_expr_dialog
            .as_ref()
            .unwrap()
            .completion
            .warning()
            .is_none()),
        "the covered dialog's suggestions must refresh from the reload, \
         not only from its own next keystroke"
    );
}
