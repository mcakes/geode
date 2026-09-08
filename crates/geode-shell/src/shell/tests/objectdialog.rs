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

/// The browse stage: the action lists the views, in normal mode, with
/// the row a user overrode marked as such — and a bare letter does NOT
/// reach the filter, which is the whole reason the dialog opens blurred.
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
    // The marker that decides whether the edit stage offers a
    // destructive "Revert to desk": painted on the overridden view and
    // on no other.
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

// ---- The edit stage ---------------------------------------------------

/// A single **desk-layer** view over a real dataset, and no user layer at
/// all. That is the fixture the destination split has to be proved on:
/// every write these tests make goes to the user layer, so "the desk's
/// view was not forked" is the assertion that a user-layer `views.toml`
/// never comes into existence.
fn services_with_a_desk_view() -> ShellServices {
    let mut services = test_services();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk_snapshot.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk_snapshot.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\n",
    )
    .unwrap();
    let desk = LayerDoc {
        layer: Layer::Desk,
        name: "views".to_string(),
        file: "<test:desk>".into(),
        table: "[tree]\ndataset = \"risk_snapshot\"\ngrouping = [\"book\"]\n\
                [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n\
                [[tree.columns]]\nname = \"npv\"\n"
            .parse()
            .unwrap(),
    };
    services.config = Config::load(&ConfigSources {
        builtin: vec![datasets, desk],
        desk: None,
        user: None,
    });
    services
}

/// Open `config::views` on the desk-layer fixture with a writable user
/// config directory, and step into `tree`'s edit stage with the cursor on
/// its first column.
fn open_tree_edit_stage(
    cx: &mut gpui::TestAppContext,
    dir: &std::path::Path,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_a_desk_view(), dir, "config::views");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    // Past the `Dataset` row and onto the `Columns` list's first item.
    cx.simulate_keystrokes("j j");
    cx.run_until_parked();
    (shell, cx)
}

fn edit_draft<T>(
    shell: &Entity<ShellView>,
    cx: &gpui::VisualTestContext,
    f: impl FnOnce(&objectdialog::Draft) -> T,
) -> T {
    shell.read_with(cx, |shell, _| {
        f(shell
            .object_dialog
            .as_ref()
            .expect("the object dialog should be open")
            .draft
            .as_ref()
            .expect("the edit stage should be open"))
    })
}

/// `enter` opens the object, and the edit stage paints the fields and
/// every column as its own row — the rows `space` and `shift+j` act on.
#[gpui::test]
fn enter_opens_the_edit_stage_and_paints_every_column(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());

    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "tree".to_string()
        },
    );
    for selector in [
        "objectdialog-edit-header",
        "objectdialog-field-dataset",
        "objectdialog-field-columns",
        "objectdialog-item-book",
        "objectdialog-item-npv",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} should have painted"
        );
    }
    // The browse list is gone, not merely covered: one stage at a time.
    assert!(cx.debug_bounds("objectdialog-row-tree").is_none());
    // Nothing is dirty yet, so the bar offers no save.
    assert!(cx.debug_bounds("objectdialog-action-s").is_none());
}

/// **The assertion this whole task exists for.** Hiding a column on a
/// DESK-layer view writes `view_presentation.toml` and leaves no
/// user-layer `views.toml` at all — because a `views.toml` override would
/// fork the desk's view, and a forked view is frozen: the desk adds a
/// column next week and the trader never sees it.
#[gpui::test]
fn hiding_a_column_writes_presentation_and_does_not_fork_the_view(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());

    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    assert!(
        !edit_draft(&shell, &cx, |d| d.list_items("columns").unwrap()[0]
            .included),
        "space hides the column under the cursor"
    );

    cx.simulate_keystrokes("s");
    cx.run_until_parked();

    let presentation = std::fs::read_to_string(dir.path().join("view_presentation.toml"))
        .expect("view_presentation.toml should have been written");
    assert!(
        presentation.contains("hidden = [\"book\"]"),
        "the hidden column has to actually be in the file:\n{presentation}"
    );
    assert!(
        !dir.path().join("views.toml").exists(),
        "hiding a column must NOT fork the desk's view into a user-layer \
         views.toml — a forked view is frozen, and the desk's next column \
         would never reach this trader"
    );
    // And the dialog says so in the unconfirmed tense: the write is on the
    // background executor and the watcher has not reloaded yet.
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        notice.is_some_and(|n| n.contains("saving")),
        "the save has to acknowledge itself without claiming it landed"
    );
}

/// A field edit **stages** and writes nothing (spec §3.2). Writing on
/// every keystroke would fire the 500 ms watcher mid-edit and reload a
/// half-finished object.
#[gpui::test]
fn a_field_edit_stages_and_writes_nothing_until_save(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());

    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    assert!(
        edit_draft(&shell, &cx, |d| d.is_dirty()),
        "the change is staged in the draft"
    );
    let written: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name())
        .collect();
    assert!(
        written.is_empty(),
        "a field edit must write nothing at all, found {written:?}"
    );
    // The bar now offers the save it did not perform.
    assert!(cx.debug_bounds("objectdialog-action-s").is_some());
}

/// `escape` on a dirty draft asks first. Abandoning unsaved work has to be
/// a deliberate second keystroke, not a keystroke that silently discards.
#[gpui::test]
fn escape_on_a_dirty_draft_confirms_before_discarding(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    cx.simulate_keystrokes("space");
    cx.run_until_parked();

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_some(),
        "a dirty draft asks before it is thrown away"
    );
    assert!(
        matches!(
            dialog_state(&shell, &cx, |s| s.stage.clone()),
            objectdialog::Stage::Edit { .. }
        ),
        "and stays in the edit stage while it asks"
    );
    // The action bar is REPLACED, not joined — nothing above it moved.
    assert!(cx.debug_bounds("objectdialog-action-s").is_none());

    cx.simulate_keystrokes("n");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_none(),
        "declining leaves the draft alone"
    );
    assert!(
        edit_draft(&shell, &cx, |d| d.is_dirty()),
        "with the change still staged"
    );

    cx.simulate_keystrokes("escape enter");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse,
        "confirming discards the draft and goes back a stage"
    );
    assert!(
        std::fs::read_dir(dir.path()).unwrap().next().is_none(),
        "and a discard writes nothing"
    );
}

/// The ladder's `PreviousStage` rung, which this dialog is the design's
/// first consumer of: `escape` on a clean draft goes back a stage, and
/// only the next one closes the modal.
#[gpui::test]
fn escape_goes_back_a_stage_before_it_closes_the_dialog(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse,
        "the first escape goes back a stage, not out of the dialog"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.modal.is_some()),
        "the modal is still open"
    );
    assert!(
        cx.debug_bounds("objectdialog-row-tree").is_some(),
        "and the browse list is back"
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        shell.read_with(&cx, |s, _| s.modal.is_none()),
        "the second closes"
    );
}

/// A desk view has nothing of the user's to delete or revert, and both
/// verbs say so rather than appearing inert — and neither writes.
#[gpui::test]
fn delete_and_revert_refuse_on_an_object_no_user_layer_defines(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());

    for (key, expected) in [("d", "nothing of yours"), ("r", "no user override")] {
        cx.simulate_keystrokes(key);
        cx.run_until_parked();
        let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
        assert!(
            notice.as_deref().is_some_and(|n| n.contains(expected)),
            "{key} should have explained itself, got {notice:?}"
        );
        assert!(
            cx.debug_bounds("objectdialog-confirm").is_none(),
            "{key} must not arm a confirm it cannot carry out"
        );
    }
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
}

/// An object opened out of a **filtered** list still gets the edit
/// stage's own `escape`. With the mode left in `Filter`, `escape_step`
/// takes the `LeaveFilter` rung, which this stage does not claim — so the
/// shell's modal branch closed the whole dialog and the draft went with
/// it, unconfirmed. Found by reading the ladder, not by the tests above:
/// every one of them opens the object from normal mode.
#[gpui::test]
fn an_object_opened_from_filter_mode_still_escapes_back_a_stage(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_a_desk_view(), dir.path(), "config::views");

    cx.simulate_keystrokes("/ t r e e");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Filter);

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "tree".to_string()
        },
        "enter opens the object from filter mode too"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.mode),
        DialogMode::Normal,
        "and the edit stage is always normal mode — its letters are verbs"
    );
    assert!(
        !dialog_filter_is_focused(&shell, &mut cx),
        "with the field blurred to match, or `s` would type instead of save"
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse,
        "escape goes back a stage, not out of the dialog"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.modal.is_some()),
        "the modal is still open"
    );
}

/// An unbound letter in the edit stage explains itself, like every other
/// key that deliberately does nothing here (`/`, `enter`, `i`, and a
/// `space` on a row with no value). A letter that is claimed, does
/// nothing and says nothing is the precise inert keystroke the
/// interaction model exists to eliminate — and it is worse in this stage
/// than in browse, because `s`/`d`/`r` have taught the user that letters
/// act here.
#[gpui::test]
fn an_unbound_letter_in_the_edit_stage_says_it_did_nothing(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());

    cx.simulate_keystrokes("x");
    cx.run_until_parked();
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        notice.as_deref().is_some_and(|n| n.contains('x')),
        "an unbound letter must name itself rather than appearing inert, got {notice:?}"
    );
    assert!(
        cx.debug_bounds("objectdialog-notice").is_some(),
        "and the notice has to actually paint"
    );
    // Saying so is all it does: the draft is untouched and the modal stays.
    assert!(!edit_draft(&shell, &cx, |d| d.is_dirty()));
    assert!(shell.read_with(&cx, |s, _| s.modal.is_some()));
}
