//! The object dialog's browse stage (Phase 4c), through real key
//! dispatch: `config::views` lists the configured views with their
//! provenance, `j`/`k` move, `/` filters, and `escape` walks the ladder.
//!
//! The pure core's own tests (`shell::objectdialog::tests`) cover the
//! markers; these cover what only a window can show — which surface owns
//! the keystrokes, and that the rows actually paint.

use super::*;
use crate::dialogmode::DialogMode;
use crate::shell::objectdialog;

/// A `views` doc across two layers: `tree` defined by both (so its row is
/// an override) and `wide` by the builtin layer alone. Built through
/// `ConfigSources::builtin`, whose entries carry their own `layer` and
/// are pushed in slice order, so this is a real two-layer config without
/// a temp directory (see `objectdialog::tests::config_from`, which does
/// the same for the pure tests).
fn services_with_views() -> ShellServices {
    let mut services = test_services();
    let builtin = LayerDoc::builtin(
        "views",
        "[tree]\ndataset = \"risk\"\ngrouping = [\"book\"]\n\
         [[tree.columns]]\nname = \"npv\"\n\
         [wide]\ndataset = \"risk\"\n[[wide.columns]]\nname = \"npv\"\n",
    )
    .unwrap();
    let user = LayerDoc {
        layer: Layer::User,
        name: "views".to_string(),
        file: "<test:user>".into(),
        table: "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"delta01\"\n"
            .parse()
            .unwrap(),
    };
    services.config = Config::load(&ConfigSources {
        builtin: vec![builtin, user],
        desk: None,
        user: None,
    });
    services
}

fn open_views_dialog(
    cx: &mut gpui::TestAppContext,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    dialog_test_shell_with(cx, services_with_views(), "config::views")
}

fn dialog_state<T>(
    shell: &Entity<ShellView>,
    cx: &gpui::VisualTestContext,
    f: impl FnOnce(&objectdialog::ObjectDialogState) -> T,
) -> T {
    shell.read_with(cx, |shell, _| {
        f(shell
            .object_dialog
            .as_ref()
            .expect("the object dialog should be open"))
    })
}

/// The stage this task ends at: the action lists the views, in normal
/// mode, with the row a user overrode marked as such — and a bare letter
/// does NOT reach the filter, which is the whole reason the dialog opens
/// blurred.
#[gpui::test]
fn config_views_opens_in_normal_mode_and_lists_the_views(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = open_views_dialog(cx);

    assert!(
        shell.read_with(&cx, |s, _| s.modal.is_some()),
        "config::views should have opened a modal"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.mode),
        DialogMode::Normal,
        "the dialog opens in normal mode, where letters are verbs"
    );
    assert!(
        !dialog_filter_is_focused(&shell, &mut cx),
        "and leaves the shared filter blurred, or every letter would type"
    );

    // One painted row per view, keyed by the view's own name.
    // `debug_bounds` takes a `&'static str`, so the selectors are spelled
    // out rather than formatted.
    for selector in ["objectdialog-row-tree", "objectdialog-row-wide"] {
        let bounds = cx.debug_bounds(selector);
        assert!(
            bounds.is_some_and(|b| b.size.width > gpui::px(0.0) && b.size.height > gpui::px(0.0)),
            "{selector} should have painted, got {bounds:?}"
        );
    }
    // The marker that decides whether Task 5 offers a destructive
    // "Revert to desk": painted on the overridden view and on no other.
    assert!(
        cx.debug_bounds("objectdialog-overridden-tree").is_some(),
        "tree is defined by both layers, so its row is marked overridden"
    );
    assert!(
        cx.debug_bounds("objectdialog-overridden-wide").is_none(),
        "wide is defined by one layer only — marking it overridden would \
         offer to revert a view no other layer has"
    );

    // A bare letter the vocabulary does not claim is swallowed: it must
    // not reach the filter as text, and must not reach the shell either.
    cx.simulate_keystrokes("x");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.query.clone()),
        "",
        "a bare letter must not be typed into the filter in normal mode"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.modal.is_some()),
        "and must not fall through to the shell underneath the modal"
    );
}

/// Normal mode's payoff: the letters are motions. `j`/`k` move the
/// selection over the *filtered* list, and neither one types.
#[gpui::test]
fn j_and_k_move_the_selection_in_normal_mode(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = open_views_dialog(cx);
    cx.simulate_keystrokes("j");
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.selected),
        1,
        "j moves down one row"
    );
    cx.simulate_keystrokes("k");
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.selected),
        0,
        "k moves back up"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.query.clone()),
        "",
        "and neither one was typed"
    );
}

/// `/` narrows the list, and then `escape` walks the ladder one visible
/// rung at a time: leave filter keeping the query, clear the query,
/// close. A dialog that skipped a rung would close on the first escape
/// and lose the user's filter with it.
#[gpui::test]
fn slash_filters_and_escape_walks_the_ladder(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = open_views_dialog(cx);

    cx.simulate_keystrokes("/");
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.mode),
        DialogMode::Filter,
        "/ enters filter mode"
    );
    assert!(
        dialog_filter_is_focused(&shell, &mut cx),
        "and hands the field focus, or the typing would fall on the floor"
    );

    cx.simulate_keystrokes("w i d e");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.query.clone()), "wide");
    assert!(
        cx.debug_bounds("objectdialog-row-wide").is_some(),
        "the matching row stays"
    );
    assert!(
        cx.debug_bounds("objectdialog-row-tree").is_none(),
        "and the non-matching row is gone — the query must actually narrow \
         the painted list, not just be stored"
    );

    cx.simulate_keystrokes("escape");
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.mode),
        DialogMode::Normal,
        "the first escape leaves filter mode"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.query.clone()),
        "wide",
        "keeping the query applied: leaving a search leaves you on the match"
    );
    assert!(
        !dialog_filter_is_focused(&shell, &mut cx),
        "and blurs the field, or normal mode's letters would still type"
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.query.clone()),
        "",
        "the second escape clears the query"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.modal.is_some()),
        "and does not close"
    );
    assert!(
        cx.debug_bounds("objectdialog-row-tree").is_some(),
        "the hidden row comes back with the cleared query"
    );

    cx.simulate_keystrokes("escape");
    assert!(
        shell.read_with(&cx, |s, _| s.modal.is_none()),
        "the third closes"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.object_dialog.is_none()),
        "and drops the dialog's own state with it, or the shared filter's \
         change subscription would keep routing to a closed dialog"
    );
}

/// The query the user cannot see must not still be ranking the list: a
/// cleared query has to clear the `Input` itself, not only the mirrored
/// copy the rows are ranked against.
#[gpui::test]
fn clearing_the_query_clears_the_field_the_next_filter_session_sees(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = open_views_dialog(cx);
    cx.simulate_keystrokes("/ w i d e");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape escape");
    cx.run_until_parked();

    cx.simulate_keystrokes("/");
    cx.simulate_keystrokes("t");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.query.clone()),
        "t",
        "the field must have been emptied along with the mirrored query"
    );
}

/// A config domain with nothing in it says so, rather than painting an
/// empty box a user cannot tell from a broken one.
#[gpui::test]
fn a_config_with_no_views_says_so(cx: &mut gpui::TestAppContext) {
    let (_shell, mut cx) = dialog_test_shell(cx, "config::views");
    assert!(
        cx.debug_bounds("objectdialog-empty").is_some(),
        "an empty domain paints its own empty state"
    );
}
