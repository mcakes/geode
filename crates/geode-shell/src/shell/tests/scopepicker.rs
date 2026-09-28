//! Scope-picker integration: `frame::scope` (`mod+o`), the toolbar's load
//! glyph, typeahead, row clicks, undo, live saved scopes, a name vanishing
//! under the open picker, and the empty state. The fixture's default
//! modifier is Alt, so the chord is dispatched as `alt-o`.

use super::scopebar::services_with_builtin_docs;
use super::*;
use geode_core::scope::Scope;
use geode_core::scopes::SavedScopes;

/// Two saved text scopes, `asia` and `eu` — no `datasets` doc, so a bare
/// `text` scope validates trivially (see `scopebar::SCOPES_DOC`).
const SCOPES_DOC: &str = "[asia]\ntext = \"asia\"\n[eu]\ntext = \"eu\"\n";

fn open_with_scopes(
    cx: &mut gpui::TestAppContext,
) -> (
    gpui::VisualTestContext,
    Entity<ShellView>,
    Entity<crate::frame::Frame>,
) {
    let services =
        services_with_builtin_docs(vec![LayerDoc::builtin("scopes", SCOPES_DOC).unwrap()]);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    vcx.run_until_parked();
    (vcx, shell, frame)
}

fn scope_text(
    frame: &Entity<crate::frame::Frame>,
    vcx: &gpui::VisualTestContext,
) -> Option<String> {
    frame.read_with(vcx, |f, _| f.shared().scope().text.clone())
}

fn text_scope(text: &str) -> Scope {
    Scope {
        text: Some(text.to_string()),
        ..Default::default()
    }
}

/// `mod+o` opens the picker; typing narrows it and `enter` loads that
/// saved scope and closes; `mod+z` then restores the scope before it —
/// the pick went through the undoable `load_scope` path.
#[gpui::test]
fn mod_o_then_typing_and_enter_loads_the_scope_undoably(cx: &mut gpui::TestAppContext) {
    let (mut vcx, shell, frame) = open_with_scopes(cx);
    assert_eq!(scope_text(&frame, &vcx), None);

    vcx.simulate_keystrokes("alt-o");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.choice_dialog.is_some()));
    assert!(vcx.debug_bounds("scope-choice-list").is_some());
    assert!(vcx.debug_bounds("scope-choice-asia").is_some());
    assert!(vcx.debug_bounds("scope-choice-eu").is_some());
    assert!(vcx.debug_bounds("scope-hints").is_some());

    vcx.simulate_input("e");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-choice-asia").is_none());
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();

    assert_eq!(scope_text(&frame, &vcx).as_deref(), Some("eu"));
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
    assert!(shell.read_with(&vcx, |s, _| s.choice_dialog.is_none()));

    vcx.simulate_keystrokes("alt-z");
    vcx.run_until_parked();
    assert_eq!(
        scope_text(&frame, &vcx),
        None,
        "mod+z restores the prior scope"
    );
}

/// The picker opens on the saved scope equal to the frame's current one,
/// and a bare `enter` there changes nothing.
#[gpui::test]
fn the_picker_opens_on_the_current_scope_and_enter_keeps_it(cx: &mut gpui::TestAppContext) {
    let (mut vcx, shell, frame) = open_with_scopes(cx);
    dispatch_action(&shell, "scope::eu", &mut vcx);
    vcx.run_until_parked();
    let before = frame.read_with(&vcx, |f, _| f.shared().versions().scope);

    vcx.simulate_keystrokes("alt-o");
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&vcx, |s, _| s
            .choice_dialog
            .as_ref()
            .and_then(|p| p.highlighted_pick())),
        Some(crate::shell::choicedialog::Pick::Scope("eu".into()))
    );
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(scope_text(&frame, &vcx).as_deref(), Some("eu"));
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().versions().scope),
        before,
        "re-picking the current scope bumps nothing"
    );
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
}

/// A scope saved after startup (here through the reload door) is a row:
/// the picker reads the frame's live saved scopes at open, not the
/// startup action registry.
#[gpui::test]
fn a_scope_saved_after_startup_is_listed_and_loads(cx: &mut gpui::TestAppContext) {
    let (mut vcx, shell, frame) = open_with_scopes(cx);
    frame.update(&mut vcx, |f, cx| {
        let mut saved: SavedScopes = f.saved_scopes().clone();
        saved.insert("later".into(), text_scope("later"));
        assert!(f.replace_saved_scopes(saved));
        cx.notify();
    });
    vcx.run_until_parked();

    vcx.simulate_keystrokes("alt-o");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-choice-later").is_some());
    vcx.simulate_input("later");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(scope_text(&frame, &vcx).as_deref(), Some("later"));
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
}

/// Clicking the toolbar's load glyph opens the picker with the field
/// focused — typing after the click filters — and the glyph reads pressed
/// while the picker is up, back at rest once it closes.
#[gpui::test]
fn clicking_the_load_glyph_opens_a_typeable_picker(cx: &mut gpui::TestAppContext) {
    let (mut vcx, shell, frame) = open_with_scopes(cx);
    assert!(vcx.debug_bounds("scope-load-chip-open").is_none());
    let glyph = vcx
        .debug_bounds("scope-load-chip")
        .expect("the load glyph paints on an empty scope");
    vcx.simulate_click(glyph.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.choice_dialog.is_some()));
    assert!(
        vcx.debug_bounds("scope-load-chip-open").is_some(),
        "the glyph reads pressed while its picker is up"
    );

    vcx.simulate_input("as");
    vcx.run_until_parked();
    let query = shell.read_with(&vcx, |s, _| {
        s.choice_dialog.as_ref().map(|p| p.list.query().to_string())
    });
    assert_eq!(
        query.as_deref(),
        Some("as"),
        "typing after the click reaches the filter"
    );
    assert!(vcx.debug_bounds("scope-choice-eu").is_none());

    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(scope_text(&frame, &vcx).as_deref(), Some("asia"));
    assert!(shell.read_with(&vcx, |s, _| s.choice_dialog.is_none()));
    assert!(vcx.debug_bounds("scope-load-chip-open").is_none());
}

/// A row click loads that row's scope and closes.
#[gpui::test]
fn a_row_click_loads_that_scope(cx: &mut gpui::TestAppContext) {
    let (mut vcx, shell, frame) = open_with_scopes(cx);
    vcx.simulate_keystrokes("alt-o");
    vcx.run_until_parked();
    let row = vcx.debug_bounds("scope-choice-eu").expect("the eu row");
    vcx.simulate_click(row.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(scope_text(&frame, &vcx).as_deref(), Some("eu"));
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
}

/// A saved scope removed under the open picker (a `scopes.toml` reload)
/// loads nothing, closes, and says so on the status bar.
#[gpui::test]
fn picking_a_scope_removed_under_the_picker_says_so(cx: &mut gpui::TestAppContext) {
    let (mut vcx, shell, frame) = open_with_scopes(cx);
    vcx.simulate_keystrokes("alt-o");
    vcx.run_until_parked();
    frame.update(&mut vcx, |f, cx| {
        let mut saved: SavedScopes = f.saved_scopes().clone();
        saved.remove("eu");
        assert!(f.replace_saved_scopes(saved));
        cx.notify();
    });
    vcx.run_until_parked();
    vcx.simulate_input("eu");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(scope_text(&frame, &vcx), None);
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.notice),
        Some(crate::shell::choicedialog::SCOPE_GONE)
    );
}

/// With no saved scope the picker still opens and says how to save one;
/// `enter` there picks nothing and the picker stays up for `escape`.
#[gpui::test]
fn with_no_saved_scopes_the_picker_says_how_to_save_one(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("alt-o");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.choice_dialog.is_some()));
    assert!(vcx.debug_bounds("scope-empty-hint").is_some());
    // Enter is inert here, so the footer does not offer it.
    assert!(vcx.debug_bounds("scope-empty-hints").is_some());
    assert!(vcx.debug_bounds("scope-hints").is_none());
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.choice_dialog.is_some()));
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
}

/// Hovering the load glyph names the action and its chord.
#[gpui::test]
fn hovering_the_load_glyph_names_the_chord(cx: &mut gpui::TestAppContext) {
    let (mut vcx, _shell, _frame) = open_with_scopes(cx);
    let glyph = vcx.debug_bounds("scope-load-chip").expect("glyph painted");
    vcx.simulate_mouse_move(
        glyph.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.executor()
        .advance_clock(std::time::Duration::from_millis(600));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("tip-scope-load-chip").is_some());
    assert!(
        vcx.debug_bounds("tip-scope-load-chip-chord-mod+o")
            .is_some()
            || vcx
                .debug_bounds("tip-scope-load-chip-chord-alt+o")
                .is_some()
    );
}
