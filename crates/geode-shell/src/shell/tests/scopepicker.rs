//! The toolbar's load glyph as the door to the Scope dialog's Saved screen
//! on its own: filter and load, undo, live saved scopes, a row
//! double-click, the empty state and the glyph's tip. The screen itself is
//! covered from `o` in `scope_saved.rs`.

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

/// Open the Saved screen through the toolbar's load glyph.
fn open_saved(vcx: &mut gpui::VisualTestContext) {
    let glyph = vcx
        .debug_bounds("scope-load-chip")
        .expect("the load glyph paints");
    vcx.simulate_click(glyph.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

fn saved_open(shell: &Entity<ShellView>, vcx: &gpui::VisualTestContext) -> bool {
    shell.read_with(vcx, |s, _| {
        s.scope_dialog
            .as_ref()
            .is_some_and(crate::shell::scopedialog::saved_view::in_saved)
    })
}

fn visible_names(shell: &Entity<ShellView>, vcx: &gpui::VisualTestContext) -> Vec<String> {
    shell.read_with(vcx, |s, _| {
        let saved = &s.scope_dialog.as_ref().expect("dialog open").saved;
        saved
            .visible
            .iter()
            .map(|&i| saved.rows[i].name.clone())
            .collect()
    })
}

/// The load glyph opens Saved; the filter narrows it, `enter` keeps the
/// filter, a second `enter` loads that saved scope and closes; `mod+z`
/// then restores the scope before it — the load went through the undoable
/// `load_saved_scope` path.
#[gpui::test]
fn the_load_glyph_then_filter_and_enter_loads_the_scope_undoably(cx: &mut gpui::TestAppContext) {
    let (mut vcx, shell, frame) = open_with_scopes(cx);
    assert_eq!(scope_text(&frame, &vcx), None);

    open_saved(&mut vcx);
    assert!(saved_open(&shell, &vcx));
    assert_eq!(visible_names(&shell, &vcx), ["asia", "eu"]);

    vcx.simulate_keystrokes("/");
    vcx.simulate_input("e");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(visible_names(&shell, &vcx), ["eu"]);
    assert!(
        saved_open(&shell, &vcx),
        "enter in the filter only keeps it"
    );
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();

    assert_eq!(scope_text(&frame, &vcx).as_deref(), Some("eu"));
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));

    vcx.simulate_keystrokes("alt-z");
    vcx.run_until_parked();
    assert_eq!(
        scope_text(&frame, &vcx),
        None,
        "mod+z restores the prior scope"
    );
}

/// A scope saved after startup (here through the reload door) is a row:
/// the screen reads the frame's live saved scopes, not the startup action
/// registry.
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

    open_saved(&mut vcx);
    assert_eq!(visible_names(&shell, &vcx), ["asia", "eu", "later"]);
    vcx.simulate_keystrokes("/");
    vcx.simulate_input("later");
    vcx.simulate_keystrokes("enter enter");
    vcx.run_until_parked();
    assert_eq!(scope_text(&frame, &vcx).as_deref(), Some("later"));
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
}

/// A row's double-click loads that row's scope and closes.
#[gpui::test]
fn a_row_double_click_loads_that_scope(cx: &mut gpui::TestAppContext) {
    let (mut vcx, shell, frame) = open_with_scopes(cx);
    open_saved(&mut vcx);
    let row = vcx.debug_bounds("scope-saved-row-1").expect("the eu row");
    double_click(&mut vcx, row.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(scope_text(&frame, &vcx).as_deref(), Some("eu"));
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
}

/// With no saved scope the screen still opens and says how to save one;
/// `enter` there loads nothing and the screen stays up for `escape`.
#[gpui::test]
fn with_no_saved_scopes_the_saved_screen_says_how_to_save_one(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    open_saved(&mut vcx);
    assert!(saved_open(&shell, &vcx));
    assert!(vcx.debug_bounds("scope-saved-empty-scopes").is_some());
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(saved_open(&shell, &vcx));
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
}

/// Hovering the load glyph names the Saved screen; `frame::scope_saved`
/// has no default binding, so the tip names no chord (`mod+o` is
/// `frame::scope`, the Current screen).
#[gpui::test]
fn hovering_the_load_glyph_names_no_chord(cx: &mut gpui::TestAppContext) {
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
            .is_none()
            && vcx
                .debug_bounds("tip-scope-load-chip-chord-alt+o")
                .is_none(),
        "the load glyph's tip names no chord"
    );
}

/// A filter that matches nothing leaves no row: `enter` loads nothing and
/// keeps the screen open; `escape` closes it.
#[gpui::test]
fn enter_with_no_match_does_nothing_and_escape_closes(cx: &mut gpui::TestAppContext) {
    let (mut vcx, shell, frame) = open_with_scopes(cx);
    open_saved(&mut vcx);
    vcx.simulate_keystrokes("/");
    vcx.simulate_input("zzz");
    vcx.simulate_keystrokes("enter enter");
    vcx.run_until_parked();
    assert!(visible_names(&shell, &vcx).is_empty());
    assert!(saved_open(&shell, &vcx));
    assert_eq!(scope_text(&frame, &vcx), None);
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
}
