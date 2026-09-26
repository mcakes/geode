//! Object-dialog integration through real key dispatch: configured objects and
//! provenance, navigation, filtering, edit stages, and the Escape ladder. Pure adapter
//! tests cover object markers; these tests cover keyboard ownership, rendered rows, and
//! persistence through the shell.

use super::*;
use crate::dialogmode::DialogMode;
use crate::shell::objectdialog;
use crate::shell::objectdialog::FieldKind;
use crate::shell::{PICKER_KEY, SCOPES_KEY};
use geode_core::query::DistinctOutcome;
use geode_core::scope::{DimensionSelection, Scope};

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
    // `ShellServices::config_and_builtin` keeps `config` and `builtin`
    // paired, exactly as `main.rs` does — a config hot reload re-merges
    // these docs, so a fixture that set one without the other would
    // model a shell whose reload deletes its own views.
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
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

/// After Enter keeps a filter and returns to normal mode, successive Escape
/// presses clear the query and close the dialog, one transition at a time.
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

    cx.simulate_keystrokes("enter");
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.mode),
        DialogMode::Normal,
        "enter leaves filter mode"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.query.clone()),
        "wide",
        "keeping the query applied: this is how a search is applied"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse,
        "and opening nothing — the edit stage is one keystroke further on"
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

/// Escape restores the filter-entry query in both list state and Input,
/// returns to normal mode, and leaves the dialog open.
#[gpui::test]
fn escape_puts_back_the_query_filter_mode_was_entered_with(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = open_views_dialog(cx);

    // A first search, applied with `enter`.
    cx.simulate_keystrokes("/ w i d e enter");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.query.clone()), "wide");

    // A second search: the query is rubbed out to look for something
    // else, the cursor moves down the wider list that comes back — and
    // then the trader changes their mind.
    cx.simulate_keystrokes("/");
    cx.simulate_keystrokes("backspace backspace backspace backspace");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-row-tree").is_some(),
        "sanity: rubbing out the query widened the list again"
    );
    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.selected),
        1,
        "sanity: the cursor is on a row the restored query will not show"
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.mode),
        DialogMode::Normal,
        "escape leaves filter mode"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.query.clone()),
        "wide",
        "and puts back the query it was entered with"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.selected),
        0,
        "with the cursor on the top match of the list that came back"
    );
    assert_eq!(
        shell.read_with(&cx, |s, cx| s.dialog_input.read(cx).value().to_string()),
        "wide",
        "the field is written from the restored query too, or the next \
         `/` would resume the abandoned search"
    );
    assert!(
        cx.debug_bounds("objectdialog-row-wide").is_some(),
        "and the row the first search found is on screen again"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.modal.is_some()),
        "reverting a search never closes the dialog"
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
    // Enter keeps the query; Escape then clears it from normal mode.
    // Escape directly from filter mode would instead restore the entry query.
    cx.simulate_keystrokes("enter escape");
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
    desk_view_services(&[])
}

/// The desk fixture plus user-layer documents keyed by document name. Adding only
/// `view_presentation` models a hidden column whose view definition still belongs to
/// the desk.
fn desk_view_services(extra: &[(&str, &str)]) -> ShellServices {
    let mut services = test_services();
    let mut layered = desk_view_docs();
    for (name, text) in extra {
        layered.push(LayerDoc {
            layer: Layer::User,
            name: (*name).to_string(),
            file: "<test:user>".into(),
            table: text.parse().expect("fixture TOML parses"),
        });
    }
    // See `services_with_views` on why the two travel together.
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: layered,
        desk: None,
        user: None,
    });
    services
}

/// Shared desk documents: one dataset and one view. Reuse them when varying config
/// sources so tests keep the same desk definition. `delta01` belongs to the dataset but
/// not the view, providing an available column without another dataset.
fn desk_view_docs() -> Vec<LayerDoc> {
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk_snapshot.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk_snapshot.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
         [risk_snapshot.columns.delta01]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
    )
    .unwrap();
    let desk = LayerDoc {
        layer: Layer::Desk,
        name: "views".to_string(),
        file: "<test:desk>".into(),
        // The desk sets `npv`'s label so clearing a column override has an inherited
        // value to reveal.
        table: "[tree]\ndataset = \"risk_snapshot\"\ngrouping = [\"book\"]\n\
                [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n\
                [[tree.columns]]\nname = \"npv\"\nlabel = \"NPV\"\n"
            .parse()
            .unwrap(),
    };
    vec![datasets, desk]
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
    // The stage opens on its first column item, skipping the inert Dataset choice and
    // Columns header.
    (shell, cx)
}

/// Let the debounced config write reach disk.
///
/// `objectdialog::apply` applies an edit to memory on the keystroke and
/// queues the file behind a [`apply::WRITE_DEBOUNCE`] timer; a test that
/// asserts on FILES has to close that window. The margin is deliberate
/// and small: the watcher's own poll is 500 ms, so one flush never
/// advances the clock far enough to make the reload fire by accident —
/// the tests that want the reload run it explicitly.
fn flush_config_write(cx: &mut gpui::VisualTestContext) {
    cx.executor()
        .advance_clock(objectdialog::apply::WRITE_DEBOUNCE + std::time::Duration::from_millis(10));
    cx.run_until_parked();
}

/// The merged `view_presentation` table for `object`, as the live config
/// holds it — the in-memory truth an instant edit has to have moved
/// before any file exists.
fn presentation_of(
    shell: &Entity<ShellView>,
    cx: &gpui::VisualTestContext,
    object: &str,
) -> Option<toml::Value> {
    shell.read_with(cx, |shell, _| {
        shell
            .services
            .config
            .doc("view_presentation")
            .and_then(|doc| doc.value.get(object))
            .cloned()
    })
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

/// The mode pill lives in the shared modal title row and remains visible in browse and
/// edit stages.
#[gpui::test]
fn the_object_dialogs_pill_sits_in_the_title_row_in_both_stages(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_a_desk_view(), dir.path(), "config::views");

    let assert_in_title_row = |cx: &mut gpui::VisualTestContext, stage: &str| {
        let pill = cx
            .debug_bounds("dialog-mode-pill-normal")
            .unwrap_or_else(|| panic!("{stage}: the pill should paint"));
        let title = cx
            .debug_bounds("shell-modal-title")
            .unwrap_or_else(|| panic!("{stage}: the title should paint"));
        assert!(
            (pill.origin.y - title.origin.y).abs() < title.size.height,
            "{stage}: the pill should share the title's row"
        );
    };

    assert_in_title_row(&mut cx, "browse");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_in_title_row(&mut cx, "edit");
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
        // An available column: present in the dataset, absent from the view.
        "objectdialog-item-delta01",
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

/// The trader-visible half of the reload bug: a save writes a config
/// file, the 500 ms watcher reloads because of it, and the desk's views
/// must still be there afterwards. They were not — the reload rebuilt the
/// builtin layer instead of reusing the one the app started with, so the
/// first save of a session emptied the browse list ("no views are
/// configured"), the edit appeared to do nothing, and only a restart
/// brought the views back.
///
/// This drives the edit through real keys, lets the debounced write land,
/// and then runs the two steps the watcher schedules
/// (`reload::load_config` off the live `services.builtin`, then
/// `apply_reload`) — its timer cannot be advanced from a gpui test, see
/// `ShellView::apply_reload`'s doc comment.
///
/// It also pins the written file across the reload: `hidden` is what the
/// trader asked for, and a reload must not be able to launder it away.
#[gpui::test]
fn the_reload_an_edit_triggers_leaves_the_views_and_the_hidden_column_intact(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());

    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    flush_config_write(&mut cx);

    let builtin = shell.read_with(&cx, |shell, _| shell.services.builtin.clone());
    let reloaded = crate::reload::load_config(builtin, None, Some(dir.path().to_path_buf()));
    shell.update(&mut cx, |shell, cx| shell.apply_reload(reloaded, cx));

    // Back to the browse list: the view the trader just edited is still
    // listed, from the same layer as before.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let rows = shell.read_with(&cx, |shell, _| {
        objectdialog::Domain::Views.objects(&shell.services.config)
    });
    assert!(
        rows.iter().any(|row| row.name == "tree"),
        "the reload the save itself triggered dropped the desk's views — \
         the dialog now says none are configured and the trader's edit \
         looks like it did nothing: {rows:?}"
    );

    let presentation = std::fs::read_to_string(dir.path().join("view_presentation.toml"))
        .expect("view_presentation.toml should have been written");
    assert!(
        presentation.contains("[tree.columns.book]") && presentation.contains("hidden = true"),
        "the hidden column must survive the reload the edit triggered:\n{presentation}"
    );
}

/// **Hazard 2, decided by proof rather than by suppression.** The 500 ms
/// watcher WILL see the file this dialog just wrote. That reload must not
/// fight the edit — and for a **value-setting** edit, the one this test
/// makes, it cannot even cost a fan-out: it produces documents identical
/// to the ones memory already holds, so every `changed(..)` predicate in
/// `apply_reload` answers false and nothing is rebuilt, re-emitted or
/// closed. (The one case where the documents do differ is a *removal*
/// against a doc with no user-layer file yet, which gains one on disk and
/// none in memory — `apply`'s module header states it; the reload is still
/// inert in value, it just is not free.)
///
/// This asserts the identity the proof rests on: the layered documents
/// before the self-triggered reload and after it are the same documents,
/// field for field — including the `file` path and the `config_version`
/// stamp of a user-layer document memory created without ever reading it
/// back. Get either of those wrong and the reload silently becomes a
/// real one: a `ConfigReloaded` emit, every tile requerying, a beat after
/// a keystroke that had already finished.
#[gpui::test]
fn the_watchers_reload_of_our_own_write_changes_nothing(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());

    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    flush_config_write(&mut cx);

    let before = layered_fingerprint(&shell, &cx);
    let builtin = shell.read_with(&cx, |shell, _| shell.services.builtin.clone());
    let reloaded = crate::reload::load_config(builtin, None, Some(dir.path().to_path_buf()));
    shell.update(&mut cx, |shell, cx| shell.apply_reload(reloaded, cx));
    let after = layered_fingerprint(&shell, &cx);

    assert_eq!(
        before, after,
        "the reload our own write triggers has to be a no-op — if these differ, \
         `apply_reload` sees a change and rebuilds the world a beat after the keystroke"
    );
}

/// Every layered document of the two docs a Views edit can touch, as the
/// tuple `shell::docs_equal` compares — the thing that has to be
/// identical across a self-triggered reload.
fn layered_fingerprint(
    shell: &Entity<ShellView>,
    cx: &gpui::VisualTestContext,
) -> Vec<(Layer, String, std::path::PathBuf, toml::Table)> {
    shell.read_with(cx, |shell, _| {
        ["views", "view_presentation"]
            .into_iter()
            .flat_map(|name| shell.services.config.layered_docs(name))
            .map(|d| (d.layer, d.name.clone(), d.file.clone(), d.table.clone()))
            .collect()
    })
}

/// Hiding a desk-view column writes only `view_presentation.toml`. It must not create a
/// user view definition, which would freeze the desk's column membership instead of
/// inheriting future changes.
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

    flush_config_write(&mut cx);

    let presentation = std::fs::read_to_string(dir.path().join("view_presentation.toml"))
        .expect("view_presentation.toml should have been written");
    assert!(
        presentation.contains("[tree.columns.book]") && presentation.contains("hidden = true"),
        "the hidden column has to actually be in the file:\n{presentation}"
    );
    assert!(
        !dir.path().join("views.toml").exists(),
        "hiding a column must NOT fork the desk's view into a user-layer \
         views.toml — a forked view is frozen, and the desk's next column \
         would never reach this trader"
    );
    // And there is nothing to announce: the change was applied on the
    // keystroke, so a notice would be reporting on something the screen
    // already shows.
    assert_eq!(dialog_state(&shell, &cx, |s| s.notice.clone()), None);
}

/// Adding an available column changes the view definition and copies a desk-owned view
/// into the user layer. The edit applies immediately and announces the copy. `delta01`
/// is the fixture's available column; hiding an existing member is covered separately
/// as presentation only.
#[gpui::test]
fn adding_an_available_column_to_a_desk_view_forks_and_says_so(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());

    // Past `book` and `npv` — both members — onto `delta01`, the
    // available block's one row.
    cx.simulate_keystrokes("j j");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-item-delta01").is_some());

    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_none(),
        "membership forks a desk view without asking"
    );
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(
        notice.contains("copied 'tree'") && notice.contains("r restores"),
        "the fork is announced, not asked about: {notice}"
    );
    flush_config_write(&mut cx);

    let written = std::fs::read_to_string(dir.path().join("views.toml"))
        .expect("the fork lands in the user layer on the keystroke's own batch");
    assert!(written.contains("name = \"delta01\""), "{written}");
    let _ = shell;
}

/// The new user copy and its overrides entry share a pending batch; reverting removes
/// both together.
#[gpui::test]
fn a_fork_records_an_override_entry_and_revert_removes_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    // A builtin view and two datasets, so stepping `dataset` is a Doc edit
    // on an object the user does not own — a fork.
    let mut services = test_services();
    let views = LayerDoc::builtin(
        "views",
        "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"book\"\n",
    )
    .unwrap();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [vol.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
    )
    .unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            views,
            datasets,
        ],
        desk: None,
        user: None,
    });
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::views");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("space"); // dataset: risk → vol, forking tree
    cx.run_until_parked();
    flush_config_write(&mut cx);
    let overrides = std::fs::read_to_string(dir.path().join("overrides.toml")).unwrap();
    assert!(overrides.contains("[\"views.tree\"]"), "{overrides}");
    assert!(
        overrides.contains("shadowed_layer = \"builtin\""),
        "{overrides}"
    );
    assert!(
        overrides.contains("dataset = \"risk\""),
        "the shadowed text is the builtin's, not the fork: {overrides}"
    );

    // The reload lands; the row is overridden, not drifted (nothing moved).
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-overridden-tree").is_some());
    assert!(cx.debug_bounds("objectdialog-drifted-tree").is_none());

    cx.simulate_keystrokes("enter r enter"); // revert to desk
    cx.run_until_parked();
    flush_config_write(&mut cx);
    let overrides = std::fs::read_to_string(dir.path().join("overrides.toml")).unwrap();
    assert!(!overrides.contains("views.tree"), "{overrides}");
    let _ = shell;
}

/// Queue stale override removals before the new override entry. Before the user copy
/// exists, stale-key detection can include the same key being created; both use one
/// `BTreeMap` entry, so the fresh value must be inserted last.
#[gpui::test]
fn the_forks_own_entry_wins_over_its_stale_twin(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let views = LayerDoc::builtin(
        "views",
        "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"book\"\n",
    )
    .unwrap();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [vol.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
    )
    .unwrap();
    // A pre-existing entry for the exact object about to be forked,
    // describing an earlier (now-defunct) shadow — this IS the key
    // `stale_override_keys` names stale, since the user layer's own
    // `views` doc does not hold `tree` yet.
    std::fs::write(
        dir.path().join("overrides.toml"),
        "[\"views.tree\"]\nshadowed_layer = \"builtin\"\n\
         shadowed_text = \"dataset = \\\"stale\\\"\"\n",
    )
    .unwrap();
    let mut services = test_services();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            views,
            datasets,
        ],
        desk: None,
        user: Some(dir.path().to_path_buf()),
    });
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::views");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("space"); // dataset: risk → vol, forking tree
    cx.run_until_parked();
    flush_config_write(&mut cx);
    let overrides = std::fs::read_to_string(dir.path().join("overrides.toml")).unwrap();
    assert!(
        overrides.contains("[\"views.tree\"]"),
        "the fork's own entry must survive being listed alongside its \
         own stale twin: {overrides}"
    );
    assert!(
        overrides.contains("dataset = \"risk\""),
        "the surviving entry must be the fresh shadow (risk), not the \
         stale text it replaced: {overrides}"
    );
    assert!(
        !overrides.contains("\"stale\""),
        "the old shadowed text must be gone: {overrides}"
    );
    let _ = shell;
}

/// A draft with error diagnostics must not enter the pending batch. Otherwise its file
/// could be written while the merged config is rejected, leaving memory and disk
/// inconsistent.
///
/// Inject a diagnostic directly because this fixture's view validation emits only
/// warnings. `shift+j` reaches the commit gate without revalidating first, preserving
/// the injected error; toggle commands would recompute diagnostics and erase it before
/// the gate.
#[gpui::test]
fn an_edit_the_reader_rejects_does_not_join_the_batch(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());

    shell.update(&mut cx, |shell, _| {
        shell
            .object_dialog
            .as_mut()
            .unwrap()
            .draft
            .as_mut()
            .unwrap()
            .diagnostics = vec![geode_core::config::Diagnostic {
            severity: geode_core::config::Severity::Error,
            layer: None,
            file: None,
            message: "dataset 'nope' does not exist".to_string(),
            path: None,
        }];
    });

    // `shift+j` still moves the item in the draft — the keystroke is not
    // swallowed and the trader's own action stays visible — but it must
    // not queue anything to write.
    cx.simulate_keystrokes("shift-j");
    cx.run_until_parked();
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.list_items("columns").unwrap()[0]
            .name
            .clone()),
        "npv",
        "the reorder itself still happens — a blocked edit must not lose \
         the keystroke that produced it"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.pending_config_write.is_none()),
        "an edit an error diagnostic rejects must never join the batch"
    );
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        notice.as_deref().is_some_and(|n| n.contains("nope")),
        "the notice has to name what the reader objected to, got {notice:?}"
    );

    flush_config_write(&mut cx);
    assert!(
        std::fs::read_dir(dir.path()).unwrap().next().is_none(),
        "no file may appear: the write must not fire for an edit that \
         never joined the batch"
    );
}

/// **The other half of the rule: a warning must never block.**
///
/// A view naming a dataset the schema no longer has is exactly the
/// reachable, by-design case (`views::validate`'s dataset check) — a
/// desk renaming a column produces this, and the whole point of it
/// being a `Warning` rather than an `Error` is that a trader's personal
/// `view_presentation.toml` must still be editable and saveable through
/// it. If the gate ever widened from "has an error" to "has any
/// diagnostic", this is the test that would catch it: the view's
/// diagnostic is present (and stays present) from the moment the draft
/// is built, entirely through real keys, with no direct `Draft` access.
#[gpui::test]
fn an_edit_with_only_warnings_still_joins_the_batch(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk_snapshot.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk_snapshot.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\n",
    )
    .unwrap();
    // `tree`'s dataset names nothing in the schema above — the desk-rename
    // shape `views::validate` warns on rather than errors on.
    let desk = LayerDoc {
        layer: Layer::Desk,
        name: "views".to_string(),
        file: "<test:desk>".into(),
        table: "[tree]\ndataset = \"a_renamed_dataset\"\ngrouping = [\"book\"]\n\
                [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n\
                [[tree.columns]]\nname = \"npv\"\n"
            .parse()
            .unwrap(),
    };
    let mut services = test_services();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![datasets, desk],
        desk: None,
        user: None,
    });
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::views");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        edit_draft(&shell, &cx, |d| d.diagnostics.iter().any(|diag| diag
            .severity
            == geode_core::config::Severity::Warning
            && diag.message.contains("a_renamed_dataset"))),
        "the fixture has to actually start with the by-design warning"
    );
    assert!(
        !edit_draft(&shell, &cx, |d| d
            .diagnostics
            .iter()
            .any(|diag| diag.severity == geode_core::config::Severity::Error)),
        "and nothing about it may be an error"
    );

    // This fixture has multiple dataset choices, making Dataset a cursor stop. One j
    // skips the Columns header to the first member; space hides that column.
    cx.simulate_keystrokes("j space");
    cx.run_until_parked();
    assert!(
        shell.read_with(&cx, |shell, _| shell.pending_config_write.is_some()),
        "a warning-only draft must still be able to join the batch"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()),
        None,
        "the edit applied cleanly — there is nothing to announce"
    );

    flush_config_write(&mut cx);
    let text = std::fs::read_to_string(dir.path().join("view_presentation.toml"))
        .expect("a warning must not have stopped the write");
    assert!(
        text.contains("[tree.columns.book]") && text.contains("hidden = true"),
        "{text}"
    );
}

/// Field edits update the dialog immediately, while merged config and file writes
/// follow together after the debounce. Deferring `ConfigReloaded` avoids requerying
/// every blotter tile at keyboard repeat rate.
#[gpui::test]
fn a_field_edit_shows_instantly_and_the_config_and_file_follow_together(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());

    cx.simulate_keystrokes("space");
    cx.run_until_parked();

    // The dialog, on the keystroke.
    assert!(
        !edit_draft(&shell, &cx, |d| d.list_items("columns").unwrap()[0]
            .included),
        "the row the trader just changed has to show the change immediately"
    );
    // And there is no save row to press, because there is nothing to save.
    assert!(cx.debug_bounds("objectdialog-action-s").is_none());

    // The rest of the world has not been disturbed yet — neither the
    // merged config nor the file.
    assert_eq!(
        presentation_of(&shell, &cx, "tree"),
        None,
        "the fan-out is debounced with the write: a keystroke must not \
         make every tile requery"
    );
    assert!(
        std::fs::read_dir(dir.path()).unwrap().next().is_none(),
        "and it must not touch the file either"
    );

    flush_config_write(&mut cx);

    let applied = presentation_of(&shell, &cx, "tree")
        .expect("the debounced flush has to reach the merged config");
    assert_eq!(
        applied
            .get("columns")
            .and_then(|v| v.get("book"))
            .and_then(|v| v.get("hidden"))
            .and_then(|v| v.as_bool()),
        Some(true),
        "hiding a column has to reach the merged config, got {applied:?}"
    );
    let text = std::fs::read_to_string(dir.path().join("view_presentation.toml"))
        .expect("and the file, on the same timer");
    assert!(
        text.contains("[tree.columns.book]") && text.contains("hidden = true"),
        "{text}"
    );
}

/// **Applying stays singular, and the fan-out rides the write's timer.**
/// The edit's `Config` reaches the screen through
/// `hot_reload::apply_reload` — the one applier the 500 ms watcher uses —
/// and not through a second path of the dialog's own; and three
/// keystrokes inside one debounce window produce exactly **one**
/// application, not three.
///
/// `ShellEvent::ConfigReloaded` is what proves both halves, and it is the
/// reason this matters rather than a tidiness argument: the bridge turns
/// that event into the `ViewSpec`s the data thread runs on
/// (`geode_core::config::load_views`), so every emission is every blotter
/// tile re-deriving and requerying. An edit that assigned
/// `services.config` directly would repaint this dialog perfectly and
/// leave those tiles on the old view; an edit that applied per keystroke
/// would requery them at the OS key-repeat rate.
#[gpui::test]
fn the_config_fan_out_is_debounced_and_goes_through_the_one_applier(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());

    let fired = std::rc::Rc::new(std::cell::RefCell::new(0usize));
    let f = fired.clone();
    cx.update(|_, cx| {
        cx.subscribe(&shell, move |_, event: &ShellEvent, _| {
            if matches!(event, ShellEvent::ConfigReloaded) {
                *f.borrow_mut() += 1;
            }
        })
        .detach();
    });

    // Three real edits, inside one window: hide `book`, hide `npv`,
    // unhide `book`.
    cx.simulate_keystrokes("space j space k space");
    cx.run_until_parked();
    assert_eq!(
        *fired.borrow(),
        0,
        "not one keystroke may fan out on its own — that is a tile requery each"
    );

    flush_config_write(&mut cx);
    assert_eq!(
        *fired.borrow(),
        1,
        "hiding a column changes the ViewSpecs every tile runs on, so the edit \
         has to go through the applier that tells the rest of the app — once"
    );
}

/// **The edit merges in memory and never reads disk.** Proved by putting
/// a decoy `views.toml` in the user directory that the running config has
/// never read: if the edit path went back to the loader's disk half, the
/// decoy would merge in and `tree` would suddenly select `decoy_dataset`.
///
/// This is the whole shape of the design — "if I change a value I have to
/// write to disk, read from disk and save to memory instead of just
/// moving the memory directly?" — and it is invisible to every other
/// assertion here, because a round trip through disk produces the same
/// value in the end. Only a disk that disagrees with memory can tell them
/// apart.
#[gpui::test]
fn an_edit_merges_in_memory_without_reading_disk(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    std::fs::write(
        dir.path().join("views.toml"),
        "config_version = 1\n[tree]\ndataset = \"decoy_dataset\"\n",
    )
    .unwrap();

    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    // The merge rides the debounce with the write (see the fan-out test);
    // this is the flush that performs it, and the decoy is what proves it
    // merged the documents in hand rather than re-reading the directory.
    flush_config_write(&mut cx);

    let dataset = shell.read_with(&cx, |shell, _| {
        shell
            .services
            .config
            .get("views", "tree.dataset")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    });
    assert_eq!(
        dataset.as_deref(),
        Some("risk_snapshot"),
        "the edit re-read the user directory instead of merging the documents \
         it was already holding — a file nothing had loaded became live"
    );
    // The edit itself still applied, through the one merge.
    assert!(
        presentation_of(&shell, &cx, "tree").is_some(),
        "and the change the keystroke made is in the merged config"
    );
}

/// **Hazard 1.** A background write that fails leaves memory ahead of
/// disk — a trader looking at a value that is not persisted, with nothing
/// on screen saying so. The in-memory change reverts, and the notice says
/// why.
///
/// The failure is the real one this write door produces: an existing file
/// that does not parse is refused *untouched* (`config_write::edit` —
/// a user's hand-edited file, however broken, is theirs).
#[gpui::test]
fn a_failed_write_reverts_the_in_memory_change_and_says_so(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    // Unparseable, and never loaded by this shell — so memory applies the
    // edit happily and only the write can discover the problem.
    std::fs::write(dir.path().join("view_presentation.toml"), "[tree\nhidden =").unwrap();

    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    assert!(
        !edit_draft(&shell, &cx, |d| d.list_items("columns").unwrap()[0]
            .included),
        "the dialog shows the change immediately, as it does for any edit"
    );

    // The flush applies to memory and *then* writes, so the failure
    // happens with the change already live — which is the hazard.
    flush_config_write(&mut cx);

    assert_eq!(
        presentation_of(&shell, &cx, "tree"),
        None,
        "a failed write has to take the in-memory change back out, or the \
         trader is looking at a value that is not persisted anywhere"
    );
    assert!(
        edit_draft(&shell, &cx, |d| d.list_items("columns").unwrap()[0]
            .included),
        "and the row has to paint the reverted value, not the refused one"
    );
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        notice.as_deref().is_some_and(|n| n.contains("reverted")),
        "and it has to say so rather than fail silently, got {notice:?}"
    );
    // The user's broken file is still their broken file.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("view_presentation.toml")).unwrap(),
        "[tree\nhidden =",
    );
}

/// **CRITICAL: an edit made while a write is in flight must not be
/// erased by that write's completion.**
///
/// Every keystroke folds its change into one pending batch and bumps a
/// sequence; the flush that wakes holding the current sequence owns the
/// batch. Every completion has to respect that sequence too. Clearing the
/// batch unconditionally loses any edit that arrived while the write was
/// in flight: the older write completes, erases the batch, and the newer
/// edit's own flush finds nothing to do — so it reaches neither memory
/// nor disk, and the watcher (woken by the write that *did* land) then
/// reverts memory to the older on-disk state. The trader's change
/// disappears with nothing on screen having said so.
///
/// Inject stale success and failure completions against the real pending batch,
/// then let its current flush reach disk. This makes the interleaving explicit
/// without depending on background executor timing.
#[gpui::test]
fn a_stale_write_completion_does_not_erase_a_newer_edit(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    let file = dir.path().join("view_presentation.toml");

    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    let seq = shell.read_with(&cx, |shell, _| {
        shell
            .pending_config_write
            .as_ref()
            .expect("the keystroke has to have queued a batch");
        shell.config_write_seq
    });

    // An older flush completing successfully, exactly as it would if this
    // keystroke had landed while that flush's write was in flight.
    shell.update(&mut cx, |shell, cx| {
        objectdialog::apply::finish_flush(shell, seq.wrapping_sub(1), Ok(()), None, cx);
        objectdialog::apply::finish_flush(
            shell,
            seq.wrapping_sub(1),
            Err("stale failure".into()),
            None,
            cx,
        );
        assert!(
            shell.config_write_error.is_none(),
            "stale failure must not replace current status"
        );
    });
    assert!(
        shell.read_with(&cx, |shell, _| shell.pending_config_write.is_some()),
        "a superseded flush's completion must not clear the batch a newer \
         edit is sitting in — that edit would reach neither memory nor disk"
    );

    flush_config_write(&mut cx);

    let text = std::fs::read_to_string(&file)
        .expect("the batch a stale completion left alone still has to be written");
    assert!(
        text.contains("[tree.columns.book]") && text.contains("hidden = true"),
        "{text}"
    );
    assert!(
        presentation_of(&shell, &cx, "tree").is_some(),
        "and it has to have been applied, not just written"
    );
    // The flush that DID own the batch clears it, so a later edit starts
    // a fresh one rather than rewriting this object forever.
    assert!(
        shell.read_with(&cx, |shell, _| shell.pending_config_write.is_none()),
        "the owning flush still has to clear what it wrote"
    );
}

/// **A broken config file elsewhere must not silently disable editing.**
///
/// `reload::decide` rejects any `Config` holding an error diagnostic, and
/// carrying the previous config's diagnostics into an edit's config fed
/// exactly that: one unparseable `*.toml` present at startup made every
/// dialog edit a no-op in memory **while the file write still fired**, so
/// memory and disk diverged and nothing said why. The trader most likely
/// to open a config dialog is precisely the one with a broken config
/// file.
///
/// Those diagnostics describe files that were **skipped** — they
/// contributed no documents — so they are not diagnostics of the
/// documents an edit re-merges, and an edit does not carry them. Last-good
/// still guards what it is for: a diagnostic the edit's own documents
/// produce (a refused `keymap.mod`, say) still rejects, because
/// `apply_reload` derives that from the documents themselves.
#[gpui::test]
fn an_edit_applies_even_when_another_config_file_is_broken(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("broken.toml"),
        "this is = = not toml
",
    )
    .unwrap();

    let mut services = test_services();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: desk_view_docs(),
        desk: None,
        user: Some(dir.path().to_path_buf()),
    });
    assert!(
        services
            .config
            .diagnostics
            .iter()
            .any(|d| d.severity == geode_core::config::Severity::Error),
        "the fixture has to actually start with a broken config file"
    );

    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::views");
    // Enter opens editing on the first column item, skipping the two inert rows above
    // it.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    flush_config_write(&mut cx);

    assert!(
        presentation_of(&shell, &cx, "tree").is_some(),
        "an unrelated broken file must not make every edit a silent no-op — \
         the write fires either way, so memory and disk would diverge"
    );
    let text = std::fs::read_to_string(dir.path().join("view_presentation.toml"))
        .expect("and the file is written, as it always was");
    assert!(text.contains("hidden"), "{text}");
}

/// **A write that fails after the dialog closed still reports itself.**
///
/// `PendingConfigWrite` lives on `ShellView` precisely so a write survives
/// the dialog that started it — a trader can close the dialog inside the
/// debounce window. That makes "the dialog's notice says so" untrue on
/// exactly the path the design exists to cover, so the failure also lands
/// in the status bar, where a closed dialog can still be seen.
#[gpui::test]
fn a_write_that_fails_after_the_dialog_closed_still_reports_itself(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    std::fs::write(dir.path().join("view_presentation.toml"), "[tree\nhidden =").unwrap();

    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    // Out of the edit stage, then out of the dialog entirely — all still
    // inside the debounce window.
    cx.simulate_keystrokes("escape escape");
    cx.run_until_parked();
    assert!(
        shell.read_with(&cx, |s, _| s.modal.is_none()),
        "the dialog is closed before the write is even attempted"
    );

    flush_config_write(&mut cx);

    let reported = shell.read_with(&cx, |s, _| s.config_write_error.clone());
    assert!(
        reported
            .as_deref()
            .is_some_and(|m| m.contains("view_presentation")),
        "a failure with no dialog open has to reach somewhere the trader can \
         see it, got {reported:?}"
    );
    assert!(
        cx.debug_bounds("config-write-error").is_some(),
        "and the status bar has to actually paint it"
    );
}

/// An empty presentation overlay removes the object's user entry in memory and on disk.
/// Hiding and unhiding a column must not leave a bare table that readers diagnose as
/// stale. This applies to overlay destinations: an empty rendering for a definition
/// must not remove the object, because absence there means inheritance.
#[gpui::test]
fn unhiding_the_last_column_removes_the_object_rather_than_writing_an_empty_table(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());

    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    flush_config_write(&mut cx);
    assert!(presentation_of(&shell, &cx, "tree").is_some());

    // Back where it started: nothing of the trader's is left to record.
    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    flush_config_write(&mut cx);
    assert_eq!(
        presentation_of(&shell, &cx, "tree"),
        None,
        "an empty presentation is an absence in memory, not an empty table"
    );

    let text = std::fs::read_to_string(dir.path().join("view_presentation.toml")).unwrap();
    assert!(
        !text.contains("[tree]"),
        "and an absence on disk too — a bare `[tree]` is the artefact this \
         ruling exists to make unwritable:\n{text}"
    );
}

/// A held key must not thrash the file. Three edits inside the debounce
/// window touch disk zero times; the window closing writes the final
/// state once.
#[gpui::test]
fn edits_inside_the_debounce_window_coalesce_into_one_write(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());

    // Hide `book`, hide `npv`, unhide `book` — three applied edits.
    cx.simulate_keystrokes("space j space k space");
    cx.run_until_parked();
    assert!(
        std::fs::read_dir(dir.path()).unwrap().next().is_none(),
        "not one of them may have reached the file yet"
    );

    flush_config_write(&mut cx);
    let text = std::fs::read_to_string(dir.path().join("view_presentation.toml"))
        .expect("the coalesced write has to land");
    assert!(
        text.contains("[tree.columns.npv]") && text.contains("hidden = true"),
        "and it has to be the FINAL state, not the first edit of the run:\n{text}"
    );
    assert!(
        !text.contains("[tree.columns.book]"),
        "book ended the run unhidden, its desk default, so it needs no \
         table at all:\n{text}"
    );
    // Memory and the file agree, which is the only thing a coalesced
    // write is allowed to change about the result.
    let applied = presentation_of(&shell, &cx, "tree").expect("still personalised");
    assert_eq!(
        applied
            .get("columns")
            .and_then(|v| v.get("npv"))
            .and_then(|v| v.get("hidden"))
            .and_then(|v| v.as_bool()),
        Some(true)
    );
}

/// A definitional edit copies a desk-owned object into the user layer, freezing its
/// definition against later desk changes. Apply the edit immediately and announce the
/// copy, the shadowed layer, and the revert command; queue the write before returning
/// from the keystroke.
#[gpui::test]
fn a_definitional_change_to_a_desk_view_forks_and_says_so(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    // A second dataset, so the `Dataset` choice has somewhere to step to.
    let services = desk_view_services(&[(
        "datasets",
        "[other_snapshot.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
    )]);
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::views");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    // The cursor opens on `Dataset`, the one `Doc`-destined field.
    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_none(),
        "forking a desk view does not ask"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.pending_config_write.is_some()),
        "the fork is queued on the keystroke"
    );
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(
        notice.contains("copied 'tree' to your config")
            && notice.contains("desk")
            && notice.contains("r restores"),
        "the notice names the copy, the shadowed layer and the way back: {notice}"
    );
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.choice("dataset").map(str::to_string)),
        Some("other_snapshot".to_string()),
        "and the value stays on screen — it is applied, not pending an answer"
    );

    flush_config_write(&mut cx);
    let text = std::fs::read_to_string(dir.path().join("views.toml"))
        .expect("the fork lands in the user layer");
    assert!(text.contains("other_snapshot"), "{text}");
}

/// `escape` on the edit stage goes straight back, because there is
/// nothing unsaved to discard. The staged model asked first — it had to,
/// since one `escape` would have thrown away every change since the
/// object was opened. With every edit applied on its own keystroke, that
/// question is about a state that cannot arise, and asking it anyway
/// would teach a trader that their changes might not have landed.
#[gpui::test]
fn escape_leaves_the_edit_stage_with_nothing_to_discard(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    cx.simulate_keystrokes("space");
    cx.run_until_parked();

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_none(),
        "there is nothing to confirm: the edit already applied"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse,
        "escape goes back a stage"
    );
    // And the edit outlives the stage it was made in: the flush queued
    // before the stage closed still applies and still writes.
    flush_config_write(&mut cx);
    assert!(presentation_of(&shell, &cx, "tree").is_some());
    let text = std::fs::read_to_string(dir.path().join("view_presentation.toml"))
        .expect("a write queued before the stage closed still has to land");
    assert!(text.contains("hidden"), "{text}");
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

/// Opening an object by clicking a filtered row enters its edit stage.
/// Escape must return to browsing without closing the whole dialog or losing
/// the draft. Filter-mode Enter only keeps the query and leaves filtering.
#[gpui::test]
fn an_object_opened_from_filter_mode_still_escapes_back_a_stage(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_a_desk_view(), dir.path(), "config::views");

    cx.simulate_keystrokes("/ t r e e");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Filter);

    click_selector(&mut cx, "objectdialog-row-tree");
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "tree".to_string()
        },
        "a click opens the object from filter mode too"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.mode),
        DialogMode::Normal,
        "and the edit stage is always normal mode — its letters are verbs"
    );
    assert!(
        !dialog_filter_is_focused(&shell, &mut cx),
        "with the field blurred to match, or `d` would type instead of act"
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

/// An unbound edit-stage letter produces a notice instead of silently doing nothing.
/// Use `z`: `x` is bound to remove a member column in this Views fixture.
#[gpui::test]
fn an_unbound_letter_in_the_edit_stage_says_it_did_nothing(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());

    cx.simulate_keystrokes("z");
    cx.run_until_parked();
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        notice.as_deref().is_some_and(|n| n.contains('z')),
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

/// A user presentation entry alone is enough for `r` to revert a desk view's
/// personalization. `d` still refuses because the definition belongs to the desk, and
/// its notice points to the available revert action.
#[gpui::test]
fn revert_undoes_a_presentation_only_override(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = desk_view_services(&[("view_presentation", "[tree]\nhidden = [\"book\"]\n")]);
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::views");

    // The browse row says so before anything is opened.
    assert!(
        cx.debug_bounds("objectdialog-overridden-tree").is_some(),
        "a user-layer presentation entry is a user override, and the row has to show it"
    );

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        notice.as_deref().is_some_and(|n| n.contains("r reverts")),
        "d must point at the verb that works rather than deny the override, got {notice:?}"
    );
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_none(),
        "d must not arm: the view itself is the desk's"
    );

    cx.simulate_keystrokes("r");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_some(),
        "r arms on a presentation-only override"
    );

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        notice
            .as_deref()
            .is_some_and(|n| n.contains("view_presentation.toml")),
        "and it reverts the file that actually holds the override, got {notice:?}"
    );
    let written = std::fs::read_to_string(dir.path().join("view_presentation.toml"))
        .expect("the presentation file is the one that gets rewritten");
    assert!(
        !written.contains("tree"),
        "the view's presentation table is gone:\n{written}"
    );
    assert!(
        !dir.path().join("views.toml").exists(),
        "and reverting presentation must not touch the view's own doc"
    );
}

/// A single view defined **only** by the user layer — no desk, no
/// builtin — so its browse row's `layer` is `Layer::User` and `d` arms
/// [`objectdialog::Confirm::Delete`] rather than pointing at `r`.
fn services_with_a_user_only_view() -> ShellServices {
    let mut services = test_services();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk_snapshot.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk_snapshot.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\n",
    )
    .unwrap();
    let user = LayerDoc {
        layer: Layer::User,
        name: "views".to_string(),
        file: "<test:user>".into(),
        table: "[mine]\ndataset = \"risk_snapshot\"\ngrouping = [\"book\"]\n\
                [[mine.columns]]\nname = \"book\"\nkind = \"dimension\"\n\
                [[mine.columns]]\nname = \"npv\"\n"
            .parse()
            .unwrap(),
    };
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![datasets, user],
        desk: None,
        user: None,
    });
    services
}

/// Confirmed removal updates memory and disk immediately, without waiting for either
/// the watcher or edit debounce. This test advances no clock: after `run_until_parked`,
/// the row must already be gone. A confirmed delete is a single operation with no
/// repeated edits to coalesce.
#[gpui::test]
fn deleting_a_user_layer_object_leaves_the_browse_list_before_the_watcher_could_fire(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_user_only_view(),
        dir.path(),
        "config::views",
    );
    assert!(
        cx.debug_bounds("objectdialog-row-mine").is_some(),
        "the browse list has to show the object before any of this starts"
    );

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_some(),
        "d arms on an object the user layer itself defines"
    );

    // The whole test: confirm, and drive the executor with nothing but
    // run_until_parked — no `advance_clock`, no simulated watcher tick.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("objectdialog-row-mine").is_none(),
        "the row must be gone from the browse list before the 500 ms watcher \
         poll could ever have run — nothing here advanced any clock"
    );
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        notice.as_deref().is_some_and(|n| n.contains("deleted")),
        "and the notice says what happened, not what is pending, got {notice:?}"
    );
    assert!(
        !notice.as_deref().unwrap_or_default().ends_with('…'),
        "the outcome is no longer pending, so the notice must not hedge \
         with an ellipsis, got {notice:?}"
    );

    // The file follows the in-memory removal.
    let written = std::fs::read_to_string(dir.path().join("views.toml"))
        .expect("the delete has to have reached disk too");
    assert!(!written.contains("mine"), "{written}");
}

/// A removal whose file write fails restores its in-memory config and reports the
/// failure. Use an unparseable on-disk file alongside valid loaded config so memory
/// removal succeeds before the writer discovers the error.
#[gpui::test]
fn a_failed_removal_reverts_the_in_memory_change_and_says_so(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = desk_view_services(&[("view_presentation", "[tree]\nhidden = [\"book\"]\n")]);
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::views");
    std::fs::write(dir.path().join("view_presentation.toml"), "[tree\nhidden =").unwrap();

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("r");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-confirm").is_some());

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert!(
        presentation_of(&shell, &cx, "tree").is_some(),
        "the failed write has to put the in-memory override back, or the \
         trader is looking at a value that is not persisted anywhere"
    );
    let reported = shell.read_with(&cx, |s, _| s.config_write_error.clone());
    assert!(
        reported.as_deref().is_some_and(|m| m.contains("reverted")),
        "and it has to say so rather than fail silently, got {reported:?}"
    );
    // The user's broken file is still their broken file.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("view_presentation.toml")).unwrap(),
        "[tree\nhidden =",
    );
}

// Grouping configuration.

/// A `datasets` doc with two dimension columns and one key column, plus
/// a `groupings` doc naming slot 3 as `dims` (in that order) — and,
/// deliberately, a real `keymap` layer carrying `BUILTIN_KEYMAP`.
///
/// That last part is the one easy to get wrong here and nowhere else in
/// this file: every other fixture in it only asserts on the *dialog*, so
/// `services_with_views`'s own `ConfigSources` never bothers with a
/// `keymap` doc — `ShellServices::keymap` (the compiled struct `ctrl+3`
/// actually resolves through) was already built once, at
/// `test_services()` time, and nothing before now needed it rebuilt.
/// This fixture's own end-to-end test does trigger a rebuild — the
/// dialog's write flush runs `apply_reload`, which recompiles the keymap
/// from `new_config.layered_docs("keymap")` unconditionally
/// (`hot_reload::apply_reload`) — so an omitted `keymap` doc would come
/// back from that flush with `ctrl+1..9` gone entirely, and the test
/// would be unable to tell "the edit never reached the frame" apart from
/// "the keystroke had nowhere to go".
fn services_with_slot_3(dims: &[&str]) -> ShellServices {
    let mut services = test_services();
    let datasets = LayerDoc::builtin(
        "datasets",
        // The position-grain measure declares the grain the three
        // dimension columns are carried by: `groupable_columns` (the
        // Groupings dialog's vocabulary) offers nothing from a dataset
        // with no grain to scan, exactly as the compiler would refuse it.
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
         [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
    )
    .unwrap();
    let quoted: Vec<String> = dims.iter().map(|d| format!("\"{d}\"")).collect();
    let groupings =
        LayerDoc::builtin("groupings", &format!("3 = [{}]\n", quoted.join(", "))).unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            datasets,
            groupings,
        ],
        desk: None,
        user: None,
    });
    services
}

/// Unconfigured grouping slots appear as rows. Opening one shows unticked dimensions,
/// and the first selection writes the slot directly into the user layer without copying
/// a desk definition.
#[gpui::test]
fn ticking_a_dimension_in_an_empty_slot_writes_it_without_asking(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_slot_3(&["book"]),
        dir.path(),
        "config::groupings",
    );
    // `debug_bounds` takes a `&'static str`, so the nine selectors are
    // spelled out rather than formatted (same reason
    // `config_views_opens_in_normal_mode_and_lists_the_views` does).
    for selector in [
        "objectdialog-row-1",
        "objectdialog-row-2",
        "objectdialog-row-3",
        "objectdialog-row-4",
        "objectdialog-row-5",
        "objectdialog-row-6",
        "objectdialog-row-7",
        "objectdialog-row-8",
        "objectdialog-row-9",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} should have painted"
        );
    }
    assert!(
        cx.debug_bounds("objectdialog-layer-1").is_none(),
        "an empty slot wears no layer"
    );
    assert!(
        cx.debug_bounds("objectdialog-layer-3").is_some(),
        "a configured slot does"
    );

    // Slot 1 is initially selected. Its edit stage skips Slot and Dimensions, so space
    // immediately ticks the first dimension.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_none(),
        "nothing to fork"
    );
    // Copying an inherited definition announces itself without confirmation. Check the
    // notice, not merely the absence of a prompt, to prove an unconfigured slot needs
    // no copy.
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        !notice.as_deref().unwrap_or_default().contains("copied"),
        "an empty slot has no layer to copy from, got {notice:?}"
    );
    // The edit is queued on the keystroke with no confirm in the way —
    // there is no desk copy for a fork question to be about. The batch
    // itself, like every other field edit, reaches `services.config`
    // and disk together behind `WRITE_DEBOUNCE` (`objectdialog::apply`'s
    // own module doc); `flush_config_write` closes that window the same
    // way every other test here that asserts on the merged config or
    // the file does.
    assert!(
        shell.read_with(&cx, |s, _| s.pending_config_write.is_some()),
        "slot 1 is queued on the keystroke, with nothing asked first"
    );
    flush_config_write(&mut cx);
    let chain = shell.read_with(&cx, |shell, _| {
        shell
            .services
            .config
            .doc("groupings")
            .and_then(|doc| doc.value.get("1"))
            .cloned()
    });
    assert!(chain.is_some(), "slot 1 reaches the live config");
    let written = std::fs::read_to_string(dir.path().join("groupings.toml")).unwrap();
    assert!(written.contains("1 = ["), "{written}");
}

#[gpui::test]
fn d_on_an_empty_slot_says_there_is_nothing_to_delete(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_slot_3(&["book"]),
        dir.path(),
        "config::groupings",
    );
    cx.simulate_keystrokes("enter d");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-confirm").is_none());
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(notice.contains("empty"), "{notice}");
}

/// Reordering a grouping slot reaches the frame through the draft, pending batch,
/// flush, reload, and slot rebuild. A later `ctrl+3` must regroup using the new
/// dimension order. Assert `Frame::active_grouping`, the value following tiles consume,
/// rather than only the persisted file.
#[gpui::test]
fn reordering_slot_3_and_pressing_ctrl_3_regroups_off_the_new_order(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = services_with_slot_3(&["book", "lhu"]);
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::groupings");

    // All nine slots are listed, with the first selected. Move to slot three before
    // opening it.
    cx.simulate_keystrokes("j j");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    // The slot opens on book, skipping Slot and Dimensions. Reorder it past lhu.
    cx.simulate_keystrokes("shift-j");
    cx.run_until_parked();

    // Grouping fields write definitions. Reordering a builtin slot creates and
    // announces a user-layer copy through the same path as other definitional edits.
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_none(),
        "reordering a builtin slot forks it into the user layer without asking"
    );
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(notice.contains("copied '3'"), "{notice}");

    // Let the debounced batch merge, apply, and write.
    flush_config_write(&mut cx);

    // Out of the dialog entirely: `ctrl+3` is a workspace binding, not
    // one the object dialog's own key handler claims, so it must not
    // still be open when the chord is pressed.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        shell.read_with(&cx, |s, _| s.modal.is_none()),
        "the dialog has to be closed for ctrl+3 to reach frame::slot_3"
    );

    cx.simulate_keystrokes("ctrl-3");
    cx.run_until_parked();

    let active = shell.read_with(&cx, |s, cx| {
        s.frame.read(cx).active_grouping().map(<[String]>::to_vec)
    });
    assert_eq!(
        active,
        Some(vec!["lhu".to_string(), "book".to_string()]),
        "a following tile must regroup off the reordered chain, not the \
         order the slot opened with"
    );

    // The file agrees too — not the assertion that matters, but the
    // whole point of the pipeline is that both do.
    let written = std::fs::read_to_string(dir.path().join("groupings.toml")).unwrap();
    assert!(
        written
            .find("lhu")
            .is_some_and(|l| written.find("book").is_some_and(|b| l < b)),
        "{written}"
    );
}

/// The confirm-and-fork step above is not incidental: `d`/`r` on a
/// Groupings slot must not panic looking for a presentation file that
/// does not exist (`Domain::presentation_doc` is `None` for Groupings) —
/// this is the regression the removal path's own generalisation guards.
#[gpui::test]
fn deleting_a_forked_slot_does_not_look_for_a_presentation_doc_that_does_not_exist(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let services = services_with_slot_3(&["book", "lhu"]);
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::groupings");

    // The first of nine slots is selected initially; navigate to slot three.
    cx.simulate_keystrokes("j j");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    // The slot opens on its first dimension, so Shift-J reorders immediately.
    cx.simulate_keystrokes("shift-j");
    cx.run_until_parked();
    flush_config_write(&mut cx);

    // The slot is now the user's own (the fork just above copied it in),
    // so `d` is live and must delete cleanly rather than panicking on a
    // presentation doc Groupings never has.
    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-confirm").is_some());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    flush_config_write(&mut cx);

    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    );
    let written = std::fs::read_to_string(dir.path().join("groupings.toml")).unwrap_or_default();
    assert!(!written.contains('3'), "{written}");
}

/// Refuse to untick a grouping slot's last dimension: empty chains are unsupported, and
/// removing the user entry would reveal an inherited chain instead of representing no
/// grouping. Assert agreement between the edit-stage rows and `Frame::active_grouping`,
/// beyond the refusal notice or file contents alone.
#[gpui::test]
fn unticking_a_slots_last_dimension_leaves_the_painted_chain_and_the_frame_agreeing(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let services = services_with_slot_3(&["book"]);
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::groupings");

    // All nine slots are listed, with slot 1 selected. Open slot 3 on book, its only
    // member; Slot and Dimensions are not cursor stops.
    cx.simulate_keystrokes("j j");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        edit_draft(&shell, &cx, |d| d.list_items("dimensions").unwrap()[0]
            .included),
        "the cursor has to be on the chain's own ticked item for this test \
         to mean anything"
    );

    cx.simulate_keystrokes("space");
    cx.run_until_parked();

    // Read now, asserted at the end: the next keystroke clears it, and the
    // assertion that matters here is the *effect*, not the message.
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_none(),
        "a declined step must not arm the fork confirm"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.pending_config_write.is_none()),
        "and it must queue nothing at all"
    );

    // Assert the chain displayed by the edit stage.
    let painted: Vec<String> = edit_draft(&shell, &cx, |d| {
        d.list_items("dimensions")
            .unwrap()
            .iter()
            .filter(|i| i.included)
            .map(|i| i.name.clone())
            .collect()
    });
    assert_eq!(painted, vec!["book".to_string()]);

    flush_config_write(&mut cx);
    assert!(
        !dir.path().join("groupings.toml").exists(),
        "nothing was queued, so nothing may be written — a `3 = []` would \
         only load as \"slot 3 is empty; ignored\", and a removal would \
         restore the layer underneath"
    );

    // Out of the dialog, so `ctrl+3` reaches `frame::slot_3`.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.simulate_keystrokes("ctrl-3");
    cx.run_until_parked();

    let active = shell.read_with(&cx, |s, cx| {
        s.frame.read(cx).active_grouping().map(<[String]>::to_vec)
    });
    assert_eq!(
        active,
        Some(painted),
        "the chain a following tile regroups by must be the chain the edit \
         stage is painting"
    );

    // And the declined keystroke said so: a key that appears inert is the
    // defect class this interaction model exists to remove.
    assert!(
        notice
            .as_deref()
            .is_some_and(|n| n.contains("at least one entry")),
        "the refusal has to explain itself, got {notice:?}"
    );
}

// Saved scopes and overwriting from the current frame.

/// A `scopes` doc with one saved scope, `mine`, selecting `book = BK001`
/// — deliberately different from whatever a test then puts on the
/// frame, so an assertion that the doc changed cannot pass by accident.
fn services_with_a_saved_scope() -> ShellServices {
    let mut services = test_services();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
         [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
    )
    .unwrap();
    let scopes =
        LayerDoc::builtin("scopes", "[mine]\n[mine.dimensions]\nbook = [\"BK001\"]\n").unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            datasets,
            scopes,
        ],
        desk: None,
        user: None,
    });
    services
}

/// [`services_with_a_saved_scope`] plus a second pickable dimension,
/// `lhu`, that `mine` does not select — an AVAILABLE row for the tick
/// tests, since the base fixture's one dimension is always the scope's
/// own selection.
fn services_with_a_saved_scope_and_an_available_dimension() -> ShellServices {
    let mut services = test_services();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
         [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
    )
    .unwrap();
    let scopes =
        LayerDoc::builtin("scopes", "[mine]\n[mine.dimensions]\nbook = [\"BK001\"]\n").unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            datasets,
            scopes,
        ],
        desk: None,
        user: None,
    });
    services
}

/// `mine`'s `book` selection, read straight off the live config the same
/// way every other test here reads a doc back — `None` when the scope or
/// the selection is gone entirely.
fn saved_scope_books(
    shell: &Entity<ShellView>,
    cx: &gpui::VisualTestContext,
) -> Option<Vec<String>> {
    shell.read_with(cx, |s, _| {
        s.services
            .config
            .doc("scopes")
            .and_then(|d| d.value.get("mine"))
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("dimensions"))
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("book"))
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
    })
}

/// `o` overwrites the selected saved scope from the current frame through the normal
/// pending-batch and reload pipeline. The frame supplies the value but remains
/// unchanged; only configuration is written.
#[gpui::test]
fn o_overwrites_the_saved_scope_with_the_frames_current_one(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = services_with_a_saved_scope();
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::scopes");

    let frame_scope = Scope {
        dimensions: vec![DimensionSelection {
            column: "book".to_string(),
            values: vec!["BK002".to_string(), "BK003".to_string()],
        }],
        ..Scope::default()
    };
    shell.update(&mut cx, |s, cx| {
        s.frame.update(cx, |f, _| {
            f.set_scope(frame_scope.clone());
        });
    });

    // Into `mine`'s edit stage — the only saved scope, so already
    // selected.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    cx.simulate_keystrokes("o");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_none(),
        "mine is builtin-owned: nothing is lost, so o writes at once"
    );
    flush_config_write(&mut cx);

    assert_eq!(
        saved_scope_books(&shell, &cx),
        Some(vec!["BK002".to_string(), "BK003".to_string()]),
        "the saved scope must now hold the frame's selection"
    );
    let written = std::fs::read_to_string(dir.path().join("scopes.toml")).unwrap_or_default();
    assert!(
        written.contains("BK002") && written.contains("BK003"),
        "{written}"
    );

    // The frame itself is unchanged — `o` writes config, never frame
    // state.
    let frame_after = shell.read_with(&cx, |s, cx| s.frame.read(cx).scope().clone());
    assert_eq!(frame_after, frame_scope);
}

/// `o` on a scope the user layer owns must confirm before acting, since
/// it destroys the saved scope's previous contents with no desk copy to
/// fall back on: pressing it alone must not touch the doc, and declining
/// (`n`) must leave `mine` exactly as it was. (On a desk-owned scope it
/// writes at once — `o_on_a_desk_owned_scope_forks_without_asking_and_says_so`.)
#[gpui::test]
fn o_confirms_before_overwriting(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = services_with_a_user_owned_scope();
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::scopes");

    let frame_scope = Scope {
        dimensions: vec![DimensionSelection {
            column: "book".to_string(),
            values: vec!["BK099".to_string()],
        }],
        ..Scope::default()
    };
    shell.update(&mut cx, |s, cx| {
        s.frame.update(cx, |f, _| {
            f.set_scope(frame_scope.clone());
        });
    });

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    cx.simulate_keystrokes("o");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_some(),
        "o must ask before overwriting"
    );

    // Declining leaves the saved scope untouched.
    cx.simulate_keystrokes("n");
    cx.run_until_parked();
    flush_config_write(&mut cx);

    assert_eq!(
        saved_scope_books(&shell, &cx),
        Some(vec!["BK001".to_string()]),
        "declining the confirm must not overwrite the saved scope"
    );
}

/// A `scopes` doc with one saved scope, `mine`, defined in the **user**
/// layer only — the non-forking twin of [`services_with_a_saved_scope`]
/// (whose `mine` is builtin-owned), so a test built on this fixture
/// exercises `arm_overwrite`'s `forks: false` branch instead.
fn services_with_a_user_owned_scope() -> ShellServices {
    let mut services = test_services();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
         [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
    )
    .unwrap();
    let user = LayerDoc {
        layer: Layer::User,
        name: "scopes".to_string(),
        file: "<test:user>".into(),
        table: "[mine]\n[mine.dimensions]\nbook = [\"BK001\"]\n"
            .parse()
            .unwrap(),
    };
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            datasets,
            user,
        ],
        desk: None,
        user: None,
    });
    services
}

/// Overwriting a builtin saved scope creates a user-layer copy immediately and
/// announces it. The original remains available through revert. Use the normal
/// browse-to-edit path so the ownership decision uses the resolved row.
#[gpui::test]
fn o_on_a_desk_owned_scope_forks_without_asking_and_says_so(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = services_with_a_saved_scope();
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::scopes");
    shell.update(&mut cx, |s, cx| {
        s.frame.update(cx, |f, _| {
            f.set_scope(Scope {
                dimensions: vec![DimensionSelection {
                    column: "book".to_string(),
                    values: vec!["BK009".to_string()],
                }],
                ..Scope::default()
            });
        });
    });

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("o");
    cx.run_until_parked();

    assert_eq!(
        dialog_state(&shell, &cx, |s| s.confirm),
        None,
        "mine is builtin-owned: nothing is lost, so o must not ask"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.pending_config_write.is_some()),
        "and the overwrite is queued on the keystroke"
    );
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(
        notice.contains("replaced")
            && notice.contains("copied 'mine'")
            && notice.contains("builtin"),
        "the notice says both what was replaced and what was copied: {notice}"
    );
}

/// The other half: `o` on a scope the user layer already owns still
/// asks, since its previous contents really are lost, and the prompt
/// claims no fork.
#[gpui::test]
fn o_on_a_user_owned_scope_still_asks_first(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = services_with_a_user_owned_scope();
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::scopes");

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("o");
    cx.run_until_parked();

    assert_eq!(
        dialog_state(&shell, &cx, |s| s.confirm),
        Some(objectdialog::Confirm::Overwrite),
        "mine is already user-owned, so o must not claim a fork"
    );
}

/// **A shell with nowhere to write must not move the draft's baseline.**
///
/// `commit_edit` resolves `ShellView::user_dir` *before* `mark_saved()`,
/// and this is the ordering that proves it: with no writable user config
/// directory nothing is queued, applied or written, so nothing has been
/// accounted for and the draft has to stay dirty. A `mark_saved()` ahead
/// of the check makes the unqueued value the baseline, so a later commit
/// treats a value that was never applied and never persisted as already
/// accounted for. Every other fixture in this file has a user directory,
/// which is why this ordering regressed unseen.
#[gpui::test]
fn an_edit_with_nowhere_to_write_leaves_the_draft_dirty(cx: &mut gpui::TestAppContext) {
    // `dialog_test_shell_with`, not `..._in_dir`: this one's `user_dir` is
    // `None`.
    let (shell, mut cx) = dialog_test_shell_with(cx, services_with_a_desk_view(), "config::views");
    // The first column item is selected on entry; Dataset and Columns are inert.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    cx.simulate_keystrokes("space");
    cx.run_until_parked();

    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        notice
            .as_deref()
            .is_some_and(|n| n.contains("no writable user config directory")),
        "a shell with nowhere to write has to say so, got {notice:?}"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.pending_config_write.is_none()),
        "and it must queue nothing"
    );
    assert!(
        edit_draft(&shell, &cx, |d| d.is_dirty()),
        "the baseline must not have moved: nothing was queued, so nothing \
         has been accounted for"
    );
    // The keystroke is still visible, which is the other half of the
    // contract — a refused write must not lose the trader's change.
    assert!(
        !edit_draft(&shell, &cx, |d| d.list_items("columns").unwrap()[0]
            .included),
        "the draft still paints the change the trader made"
    );
}

/// A `scopes` doc holding `mine` exactly as `persist_scope_to_user_config`
/// writes one — `dimensions` plus the empty `text` and `expression` keys
/// `scope_to_table` always emits. That detail is the fixture's whole
/// point: it is what makes a frame carrying the same selection render a
/// *byte-identical* table, which is the state `:scope load mine` leaves
/// the app in and the one [`services_with_a_saved_scope`] (whose `mine`
/// omits both keys) cannot reach.
fn services_with_a_scope_the_app_itself_wrote() -> ShellServices {
    let mut services = test_services();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
         [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
    )
    .unwrap();
    let scopes = LayerDoc::builtin(
        "scopes",
        "[mine]\ntext = \"\"\nexpression = \"\"\n\
         [mine.dimensions]\nbook = [\"BK001\"]\n",
    )
    .unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            datasets,
            scopes,
        ],
        desk: None,
        user: None,
    });
    services
}

/// Overwriting a saved scope already equal to the frame reports that nothing changed.
/// `commit_edit` returns `None` for both queued edits and no-ops, so the caller must
/// identify this case to give feedback.
#[gpui::test]
fn o_on_a_scope_that_already_matches_the_frame_says_so(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = services_with_a_scope_the_app_itself_wrote();
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::scopes");

    // Exactly what the saved scope already holds.
    shell.update(&mut cx, |s, cx| {
        s.frame.update(cx, |f, _| {
            f.set_scope(Scope {
                dimensions: vec![DimensionSelection {
                    column: "book".to_string(),
                    values: vec!["BK001".to_string()],
                }],
                ..Scope::default()
            });
        });
    });

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("o");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_none(),
        "builtin-owned, so o acts at once"
    );

    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        notice
            .as_deref()
            .is_some_and(|n| n.contains("already matches the frame")),
        "an o that writes nothing has to say why, got {notice:?}"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.pending_config_write.is_none()),
        "and there really was nothing to write — the draft was clean"
    );
    flush_config_write(&mut cx);
    assert!(
        !dir.path().join("scopes.toml").exists(),
        "nothing changed, so no file may appear"
    );
}

/// `commit_create` writes exactly one Doc entry, at zero debounce, and
/// never an empty presentation table for the untouched column list.
#[gpui::test]
fn commit_create_writes_one_doc_entry_immediately(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_a_desk_view(), dir.path(), "config::views");
    shell.update(&mut cx, |shell, cx| {
        let draft = objectdialog::Domain::Views.new_draft(&shell.services.config, "mine");
        let state = shell.object_dialog.as_mut().unwrap();
        state.draft = Some(draft);
        state.stage = objectdialog::Stage::Edit {
            object: "mine".to_string(),
        };
        assert_eq!(objectdialog::apply::commit_create(shell, cx), None);
    });
    cx.run_until_parked(); // no clock advance: zero debounce
    let written = std::fs::read_to_string(dir.path().join("views.toml")).unwrap();
    assert!(
        written.contains("[mine]") && written.contains("dataset = \"risk_snapshot\""),
        "{written}"
    );
    assert!(
        !dir.path().join("view_presentation.toml").exists(),
        "no overlay write for a new object"
    );
    let in_config = shell.read_with(&cx, |shell, _| {
        shell
            .services
            .config
            .doc("views")
            .and_then(|d| d.value.get("mine"))
            .is_some()
    });
    assert!(in_config);
}

// Naming, creation, and the edit stage for a new object.

/// `n` opens the name field. Enter on a valid view name creates the object, opens its
/// edit stage, and adds it to the browse list.
#[gpui::test]
fn n_creates_a_view_on_enter_and_opens_its_edit_stage(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_a_desk_view(), dir.path(), "config::views");
    cx.simulate_keystrokes("n");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Naming
    );
    assert!(cx.debug_bounds("dialog-name-row").is_some());
    assert!(
        dialog_filter_is_focused(&shell, &mut cx),
        "the name field owns the keys"
    );
    cx.simulate_input("mine");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit { object } if object == "mine"
    ));
    assert!(edit_draft(&shell, &cx, |d| d.is_new));
    assert!(cx.debug_bounds("objectdialog-new-badge").is_some());
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.choice("dataset").map(str::to_string)),
        Some("risk_snapshot".into())
    );
    // Zero debounce: on disk and in the config with no clock advance.
    let written = std::fs::read_to_string(dir.path().join("views.toml")).unwrap();
    assert!(written.contains("[mine]"), "{written}");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-row-mine").is_some(),
        "back in browse, the new row is there"
    );
}

/// Starting naming after filtering clears both the query state and the shared input.
/// They are separate buffers, and programmatic input changes do not emit the event used
/// for mirroring. A retained query must not become an invisible prefix on the new
/// object's name.
#[gpui::test]
fn n_opens_an_empty_name_field_even_after_a_browse_filter(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_a_desk_view(), dir.path(), "config::views");

    cx.simulate_keystrokes("/");
    cx.simulate_keystrokes("t r");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.query.clone()), "tr");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.query.clone()),
        "tr",
        "leaving filter mode by enter keeps the query applied"
    );

    cx.simulate_keystrokes("n");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Naming
    );
    assert_eq!(
        shell.read_with(&cx, |s, cx| s.dialog_input.read(cx).value().to_string()),
        "",
        "the name field must not open pre-filled with the old browse filter"
    );

    cx.simulate_input("ee");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit { object } if object == "ee"
    ));
    let written = std::fs::read_to_string(dir.path().join("views.toml")).unwrap();
    assert!(written.contains("[ee]"), "{written}");
    assert!(
        !written.contains("[tree]"),
        "the stale field text must not have forked the desk's tree view: {written}"
    );
}

/// A name a layer already holds must be refused: creating `tree` would
/// fork the desk's view under a verb that never said so.
#[gpui::test]
fn n_refuses_a_name_any_layer_already_holds(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_a_desk_view(), dir.path(), "config::views");
    cx.simulate_keystrokes("n");
    cx.simulate_input("tree");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Naming,
        "still naming"
    );
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(notice.contains("already exists"), "{notice}");
    assert!(!dir.path().join("views.toml").exists());
    // escape backs out with nothing written and the query gone.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| (s.stage.clone(), s.query.clone())),
        (objectdialog::Stage::Browse, String::new())
    );
}

/// The half of "a name any layer already holds" that has no row to show
/// for it: the desk dropped `gone` after the trader hid a column on it,
/// so `view_presentation.toml` still names it while no layer of
/// `views.toml` does. Creating it produced a fresh user view that
/// silently inherited the orphaned overlay — and, being user-only, `r`
/// refused it, so no verb in this dialog could clear it again.
#[gpui::test]
fn n_refuses_a_name_only_the_presentation_overlay_holds(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        desk_view_services(&[("view_presentation", "[gone]\nhidden = [\"npv\"]\n")]),
        dir.path(),
        "config::views",
    );
    assert!(
        cx.debug_bounds("objectdialog-row-tree").is_some(),
        "the desk's view is listed"
    );
    cx.simulate_keystrokes("n");
    cx.simulate_input("gone");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Naming,
        "still naming — nothing was created"
    );
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(
        notice.contains("view_presentation.toml"),
        "the notice must name the overlay entry that is in the way, not a \
         row the trader can open: {notice}"
    );
    assert!(
        !dir.path().join("views.toml").exists(),
        "no user view was written"
    );
}

/// `n` creates an empty scope and opens it with a new badge. Copying the frame's scope
/// is the separate overwrite action.
#[gpui::test]
fn n_on_scopes_creates_an_empty_scope(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    shell.update(&mut cx, |shell, cx| {
        shell.frame.update(cx, |f, _| {
            f.set_scope(Scope {
                dimensions: vec![DimensionSelection {
                    column: "book".to_string(),
                    values: vec!["BK007".to_string()],
                }],
                ..Scope::default()
            });
        });
    });
    cx.simulate_keystrokes("n");
    cx.simulate_input("today");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let written = std::fs::read_to_string(dir.path().join("scopes.toml")).unwrap();
    assert!(written.contains("[today"), "{written}");
    assert!(
        !written.contains("BK007"),
        "the frame's scope is not copied: {written}"
    );
    assert!(edit_draft(&shell, &cx, |d| d.is_new));
    assert!(edit_draft(&shell, &cx, |d| d
        .list_items("dimensions")
        .unwrap()
        .is_empty()));
}

/// `c` on a browse row copies that scope verbatim under the typed name
/// and opens the copy.
#[gpui::test]
fn c_duplicates_the_selected_scope_under_a_new_name(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    cx.simulate_keystrokes("c");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Naming
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.naming_seed.clone()),
        objectdialog::NameSeed::CopyOf("mine".to_string())
    );
    cx.simulate_input("mine2");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let written = std::fs::read_to_string(dir.path().join("scopes.toml")).unwrap();
    assert!(
        written.contains("[mine2.dimensions]") && written.contains("BK001"),
        "{written}"
    );
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit { ref object } if object == "mine2"
    ));
    assert!(edit_draft(&shell, &cx, |d| d.is_new));
}

/// A source deleted out from under an armed `c` — between the keystroke
/// that recorded its name in `naming_seed` and the `enter` that would
/// copy it — is refused with a notice, never written as a blank copy
/// under the new name. `naming_seed` records the source by NAME rather
/// than by row index for exactly this reason (see its own doc comment),
/// but the name itself can still stop resolving if the object is gone
/// from the live config by the time `enter` reads it back.
#[gpui::test]
fn c_refuses_when_the_source_vanished_before_enter(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    cx.simulate_keystrokes("c");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Naming
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.naming_seed.clone()),
        objectdialog::NameSeed::CopyOf("mine".to_string())
    );
    // `mine` vanishes from the LIVE config before `enter` — the same
    // datasets doc `services_with_a_saved_scope` uses, but an empty
    // `scopes` doc rather than one still holding `mine`.
    shell.update(&mut cx, |shell, cx| {
        let datasets = LayerDoc::builtin(
            "datasets",
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
             [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
             [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
        )
        .unwrap();
        let scopes = LayerDoc::builtin("scopes", "").unwrap();
        (shell.services.config, shell.services.builtin) =
            ShellServices::config_and_builtin(ConfigSources {
                builtin: vec![
                    LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
                    datasets,
                    scopes,
                ],
                desk: None,
                user: None,
            });
        cx.notify();
    });
    cx.run_until_parked();
    cx.simulate_input("mine2");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    // Still naming — `enter` refused rather than creating.
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Naming
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()),
        Some("'mine' is gone — nothing to copy".to_string())
    );
    // No `scopes.toml` written under the new name at all — the refusal
    // happens before `enter_edit_stage`/`commit_create` ever run.
    let written_path = dir.path().join("scopes.toml");
    if written_path.exists() {
        let written = std::fs::read_to_string(&written_path).unwrap();
        assert!(!written.contains("mine2"), "{written}");
    }
    assert!(dialog_state(&shell, &cx, |s| s.draft.is_none()));
}

/// `c` is Scopes-only for now: elsewhere it is an unbound letter.
#[gpui::test]
fn c_is_not_a_verb_on_views(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_a_desk_view(), dir.path(), "config::views");
    cx.simulate_keystrokes("c");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    );
}

// Saving the current scope opens naming from the palette action or scope-bar chip,
// seeded with the frame's scope.

fn set_frame_book_scope(shell: &Entity<ShellView>, cx: &mut gpui::VisualTestContext, book: &str) {
    shell.update(cx, |s, cx| {
        s.frame.update(cx, |f, _| {
            f.set_scope(Scope {
                dimensions: vec![DimensionSelection {
                    column: "book".to_string(),
                    values: vec![book.to_string()],
                }],
                ..Scope::default()
            });
        });
    });
}

/// `scope::save_current` opens naming with a `FromFrame` seed. Enter on a new name
/// saves the frame's selection and opens the new scope's edit stage.
#[gpui::test]
fn scope_save_current_seeds_naming_from_the_frame_and_creates_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (window, mut cx) = open_shell_with_user_dir(cx, services_with_a_saved_scope(), dir.path());
    let shell = shell_of(&window, &mut cx);
    set_frame_book_scope(&shell, &mut cx, "BK009");

    dispatch_action(&shell, "scope::save_current", &mut cx);
    cx.run_until_parked();

    assert!(
        shell.read_with(&cx, |s, _| s.modal.is_some()),
        "scope::save_current should have opened a modal"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Naming
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.naming_seed.clone()),
        objectdialog::NameSeed::FromFrame
    );

    cx.simulate_input("today");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    let written = std::fs::read_to_string(dir.path().join("scopes.toml")).unwrap();
    assert!(
        written.contains("[today.dimensions]") && written.contains("BK009"),
        "{written}"
    );
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit { ref object } if object == "today"
    ));
    assert!(edit_draft(&shell, &cx, |d| d.is_new));
}

/// An empty frame scope has nothing to save: `scope::save_current` opens
/// the dialog in browse, with a notice, and never enters naming at all.
#[gpui::test]
fn scope_save_current_with_an_empty_frame_scope_opens_browse_with_a_notice(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (window, mut cx) = open_shell_with_user_dir(cx, services_with_a_saved_scope(), dir.path());
    let shell = shell_of(&window, &mut cx);
    assert!(shell.read_with(&cx, |s, cx| s.frame.read(cx).scope().is_empty()));

    dispatch_action(&shell, "scope::save_current", &mut cx);
    cx.run_until_parked();

    assert!(shell.read_with(&cx, |s, _| s.modal.is_some()));
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse,
        "nothing to save — no naming prompt"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()),
        Some("the frame's scope is empty — nothing to save".to_string())
    );
    assert!(!dir.path().join("scopes.toml").exists());
}

/// `enter` on a name already taken is refused with the same notice
/// `n`/`c` give — `scope::save_current` reaches `create_from_name`'s one
/// name check like every other naming path.
#[gpui::test]
fn scope_save_current_refuses_a_taken_name(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (window, mut cx) = open_shell_with_user_dir(cx, services_with_a_saved_scope(), dir.path());
    let shell = shell_of(&window, &mut cx);
    set_frame_book_scope(&shell, &mut cx, "BK009");

    dispatch_action(&shell, "scope::save_current", &mut cx);
    cx.run_until_parked();
    cx.simulate_input("mine"); // the fixture's own saved scope
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Naming,
        "still naming — nothing was created"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()),
        Some("'mine' already exists — open it instead".to_string())
    );
    assert_eq!(
        saved_scope_books(&shell, &cx),
        Some(vec!["BK001".to_string()]),
        "the existing saved scope must be untouched"
    );
}

/// `save_current` itself cannot be used as a saved scope's name — it is
/// the palette action's own id (`Domain::Scopes.reserved_names()`), and
/// letting a scope claim it would shadow `input.rs`'s dispatch arm.
#[gpui::test]
fn scope_save_current_refuses_its_own_name_as_reserved(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (window, mut cx) = open_shell_with_user_dir(cx, services_with_a_saved_scope(), dir.path());
    let shell = shell_of(&window, &mut cx);
    set_frame_book_scope(&shell, &mut cx, "BK009");

    dispatch_action(&shell, "scope::save_current", &mut cx);
    cx.run_until_parked();
    cx.simulate_input("save_current");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Naming
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()),
        Some("'save_current' is reserved".to_string())
    );
    let written_path = dir.path().join("scopes.toml");
    if written_path.exists() {
        let written = std::fs::read_to_string(&written_path).unwrap();
        assert!(!written.contains("save_current"), "{written}");
    }
}

/// `escape` from a `FromFrame` naming prompt cancels like any other:
/// back to browse, nothing written.
#[gpui::test]
fn escape_from_save_current_naming_writes_nothing(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (window, mut cx) = open_shell_with_user_dir(cx, services_with_a_saved_scope(), dir.path());
    let shell = shell_of(&window, &mut cx);
    set_frame_book_scope(&shell, &mut cx, "BK009");

    dispatch_action(&shell, "scope::save_current", &mut cx);
    cx.run_until_parked();
    cx.simulate_input("today");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.naming_seed.clone()),
        objectdialog::NameSeed::Empty,
        "cancel_naming resets naming_seed like every other seed"
    );
    assert!(!dir.path().join("scopes.toml").exists());
}

/// Saving the current scope while another modal is open leaves that modal and its
/// object state untouched. A refused open must not continue by mutating the existing
/// Views dialog's naming state.
#[gpui::test]
fn scope_save_current_does_not_touch_an_already_open_dialog(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_a_desk_view(), dir.path(), "config::views");
    set_frame_book_scope(&shell, &mut cx, "BK009");

    dispatch_action(&shell, "scope::save_current", &mut cx);
    cx.run_until_parked();

    assert_eq!(
        dialog_state(&shell, &cx, |s| s.domain),
        objectdialog::Domain::Views,
        "the open dialog must still be Views, not Scopes"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse,
        "still browsing — scope::save_current must not have entered naming"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.naming_seed.clone()),
        objectdialog::NameSeed::Empty
    );
}

/// A `SCOPES_KEY` outcome reaches the Values stage; a `PICKER_KEY` one
/// never does, and a stale tag or a different column is dropped.
#[gpui::test]
fn deliver_values_routes_by_key_and_drops_stale_outcomes(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    cx.simulate_keystrokes("enter"); // open `mine`
    // The stage opens on book, skipping the inert Dimensions header.
    cx.simulate_keystrokes("enter"); // Values stage (Task 4's door)
    cx.run_until_parked();
    let tag = dialog_state(&shell, &cx, |s| s.values_tag);
    let deliver =
        |shell: &Entity<ShellView>, cx: &mut gpui::VisualTestContext, key, tag, column: &str| {
            shell.update(cx, |s, cx| {
                s.deliver_distinct(
                    DistinctOutcome {
                        key,
                        tag,
                        column: column.into(),
                        values: Ok(vec![("BK000".into(), 5), ("BK001".into(), 7)]),
                    },
                    cx,
                )
            });
            cx.run_until_parked();
        };
    let still_loading = |shell: &Entity<ShellView>, cx: &gpui::VisualTestContext| {
        edit_draft(
            shell,
            cx,
            |d| matches!(&d.fields[0].kind, FieldKind::Text(t) if t == "loading…"),
        )
    };
    deliver(&shell, &mut cx, PICKER_KEY, tag, "book");
    assert!(
        still_loading(&shell, &cx),
        "the picker's key never reaches the dialog"
    );
    deliver(&shell, &mut cx, SCOPES_KEY, tag.wrapping_sub(1), "book");
    assert!(still_loading(&shell, &cx), "a stale tag is dropped");
    deliver(&shell, &mut cx, SCOPES_KEY, tag, "lhu");
    assert!(
        still_loading(&shell, &cx),
        "another column's answer is dropped"
    );
    deliver(&shell, &mut cx, SCOPES_KEY, tag, "book");
    let names: Vec<String> = edit_draft(&shell, &cx, |d| {
        d.list_items("values")
            .unwrap()
            .iter()
            .map(|i| i.name.clone())
            .collect()
    });
    assert_eq!(names, ["BK000", "BK001"]);
    assert!(edit_draft(&shell, &cx, |d| d.list_items("values").unwrap()
        [1]
    .included));
    assert!(
        !edit_draft(&shell, &cx, |d| d.is_dirty()),
        "a delivery is not dirt"
    );
}

/// The Values stage paints value rows and their section header beneath its breadcrumb,
/// with no browse row underneath. The rendered list must match the draft that keys and
/// ticks modify.
#[gpui::test]
fn the_values_stage_paints_its_rows(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    cx.simulate_keystrokes("enter"); // open `mine`
    // The stage opens on book, skipping the inert Dimensions header.
    cx.simulate_keystrokes("enter"); // Values stage
    cx.run_until_parked();
    let tag = dialog_state(&shell, &cx, |s| s.values_tag);
    shell.update(&mut cx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: SCOPES_KEY,
                tag,
                column: "book".into(),
                values: Ok(vec![("BK000".into(), 5), ("BK001".into(), 7)]),
            },
            cx,
        )
    });
    cx.run_until_parked();
    assert!(
        matches!(
            dialog_state(&shell, &cx, |s| s.stage.clone()),
            objectdialog::Stage::Values { .. }
        ),
        "the stage really is Values, not just the state — a mismatch here \
         would mean the test proves nothing about the paint"
    );
    let item_bounds = cx.debug_bounds("objectdialog-item-BK000");
    assert!(
        item_bounds.is_some_and(|b| b.size.width > gpui::px(0.0) && b.size.height > gpui::px(0.0)),
        "the Values stage's own row should paint, got {item_bounds:?}"
    );
    assert!(
        cx.debug_bounds("objectdialog-section-members-values")
            .is_some(),
        "the Values section header should paint"
    );
    assert!(
        cx.debug_bounds("objectdialog-row-mine").is_none(),
        "no browse row should paint while the Values stage is open"
    );
}

/// `enter` on a `dimensions` row opens the Values stage and asks the
/// data for that column's values, carrying the DRAFT's scope minus the
/// column; `space` on an available row opens it too; `escape` returns to
/// the scope with the cursor on the column.
#[gpui::test]
fn entering_the_values_stage_requests_the_columns_distinct_values(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    let requested = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    cx.update(|_, cx| {
        let requested = requested.clone();
        cx.subscribe(&shell, move |_, e: &ShellEvent, _| {
            if let ShellEvent::DistinctRequested(p) = e {
                requested.borrow_mut().push(p.clone());
            }
        })
        .detach();
    });
    // The stage opens on book; another Enter opens its Values stage.
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Values { ref column, .. } if column == "book"
    ));
    let req = requested
        .borrow()
        .last()
        .cloned()
        .expect("a distinct request");
    assert_eq!(req.key, SCOPES_KEY);
    assert_eq!(req.column, "book");
    assert!(
        req.scope.dimensions.is_empty(),
        "own selection removed from a one-dimension scope"
    );
    assert_eq!(req.tag, dialog_state(&shell, &cx, |s| s.values_tag));
    assert!(edit_draft(&shell, &cx, |d| d.values() == Some("book")));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit { .. }
    ));
    assert!(edit_draft(&shell, &cx, |d| d.values().is_none()));
    assert!(
        edit_draft(&shell, &cx, |d| matches!(
            d.selected_row(),
            Some(objectdialog::EditRow::Item { .. })
        )),
        "the cursor lands on the column's own row"
    );
}

/// A tick in the Values stage writes the selection to disk through the
/// ordinary debounced path, and unticking every value removes the key.
#[gpui::test]
fn ticking_a_value_writes_the_selection_and_unticking_all_removes_it(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    // The stage opens on book; another Enter opens its Values stage.
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let tag = dialog_state(&shell, &cx, |s| s.values_tag);
    shell.update(&mut cx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: SCOPES_KEY,
                tag,
                column: "book".into(),
                values: Ok(vec![("BK000".into(), 5), ("BK001".into(), 7)]),
            },
            cx,
        )
    });
    cx.run_until_parked();
    // Delivery selects the first value, skipping the inert Values header.
    cx.simulate_keystrokes("space"); // tick BK000
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(400));
    cx.run_until_parked();
    assert_eq!(
        saved_scope_books(&shell, &cx).as_deref(),
        Some(&["BK000".to_string(), "BK001".to_string()][..])
    );
    cx.simulate_keystrokes("space"); // untick BK000
    cx.simulate_keystrokes("j");
    cx.simulate_keystrokes("space"); // untick BK001
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(400));
    cx.run_until_parked();
    assert_eq!(
        saved_scope_books(&shell, &cx),
        None,
        "an emptied selection is removed, never []"
    );
    let written = std::fs::read_to_string(dir.path().join("scopes.toml")).unwrap();
    assert!(!written.contains("book = []"), "{written}");
}

/// `ctrl+a` ticks every shown value, `ctrl+x` clears; `shift+j` refuses
/// with the no-order notice on both stages.
#[gpui::test]
fn ctrl_a_and_ctrl_x_tick_and_clear_and_reorder_is_refused(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    // The stage opens on book, skipping the inert Dimensions header.
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("shift-j");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some("selections have no order")
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let tag = dialog_state(&shell, &cx, |s| s.values_tag);
    shell.update(&mut cx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: SCOPES_KEY,
                tag,
                column: "book".into(),
                values: Ok(vec![
                    ("BK000".into(), 5),
                    ("BK001".into(), 7),
                    ("BK002".into(), 1),
                ]),
            },
            cx,
        )
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("ctrl-a");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d
        .list_items("values")
        .unwrap()
        .iter()
        .all(|i| i.included)));
    cx.simulate_keystrokes("ctrl-x");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d
        .list_items("values")
        .unwrap()
        .iter()
        .all(|i| !i.included)));
    cx.simulate_keystrokes("j");
    cx.simulate_keystrokes("shift-j");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some("selections have no order")
    );
}

/// Filtering Values updates the draft query and narrows its rendered rows. Select-all
/// then ticks only those filtered values, without modifying the independent browse
/// query.
#[gpui::test]
fn a_query_in_the_values_stage_narrows_the_rows_and_ctrl_a_ticks_only_them(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    cx.simulate_keystrokes("enter"); // open `mine`
    // The stage opens on book, skipping the inert Dimensions header.
    cx.simulate_keystrokes("enter"); // Values stage
    cx.run_until_parked();
    let tag = dialog_state(&shell, &cx, |s| s.values_tag);
    shell.update(&mut cx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: SCOPES_KEY,
                tag,
                column: "book".into(),
                // `BK001` is `mine`'s own saved selection, so it starts
                // ticked; `BK000` and `ZZ9` do not.
                values: Ok(vec![
                    ("BK000".into(), 5),
                    ("BK001".into(), 7),
                    ("ZZ9".into(), 1),
                ]),
            },
            cx,
        )
    });
    cx.run_until_parked();

    cx.simulate_keystrokes("/");
    cx.run_until_parked();
    cx.simulate_input("BK");
    cx.run_until_parked();
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.query.clone()),
        "BK",
        "the keystrokes must land on the draft's own query, not the \
         browse-stage slot `Stage::Values` has no business writing"
    );

    // The header row's label is "Values", which shares no letters with
    // "BK" — narrowed to the two matching item rows, both from the
    // field's own `rows()`, computed rather than hard-coded so this
    // assertion states the real filtering behaviour instead of a magic
    // number.
    let visible_names: Vec<String> = edit_draft(&shell, &cx, |d| {
        let rows = d.rows();
        d.visible_rows()
            .iter()
            .filter_map(|m| rows.get(m.row).map(|r| d.row_label(*r)))
            .collect()
    });
    assert_eq!(
        visible_names,
        vec!["BK000".to_string(), "BK001".to_string()],
        "the filter should narrow to the two matching values and drop \
         `ZZ9` (and the unrelated `Values` header row)"
    );

    cx.simulate_keystrokes("ctrl-a");
    cx.run_until_parked();
    let ticked: Vec<(String, bool)> = edit_draft(&shell, &cx, |d| {
        d.list_items("values")
            .unwrap()
            .iter()
            .map(|i| (i.name.clone(), i.included))
            .collect()
    });
    assert_eq!(
        ticked,
        vec![
            ("BK000".to_string(), true),
            ("BK001".to_string(), true),
            ("ZZ9".to_string(), false),
        ],
        "ctrl+a must tick only the values the filter shows — a `ZZ9` \
         ticked here means it fell back to ticking the whole list"
    );
}

/// `i` on the expression row refuses a broken expression and keeps the
/// field open; a good one is written.
#[gpui::test]
fn a_broken_expression_is_refused_and_a_good_one_is_written(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("shift-g"); // last row: expression
    cx.simulate_keystrokes("i");
    cx.simulate_input("npv >");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        edit_draft(&shell, &cx, |d| d.text_entry.is_some()),
        "the field stays open"
    );
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(notice.starts_with("expression: "), "{notice}");
    cx.simulate_input(" 0");
    cx.simulate_keystrokes("enter");
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(400));
    cx.run_until_parked();
    let written = std::fs::read_to_string(dir.path().join("scopes.toml")).unwrap();
    assert!(written.contains("expression = \"npv > 0\""), "{written}");
}

/// Delete, revert, and overwrite are unavailable in the Values stage. Each produces the
/// same notice and preserves the stage, confirmation, and draft.
#[gpui::test]
fn d_r_and_o_refuse_inside_the_values_stage(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    // The stage opens on book; another Enter opens its Values stage.
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let tag = dialog_state(&shell, &cx, |s| s.values_tag);
    shell.update(&mut cx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: SCOPES_KEY,
                tag,
                column: "book".into(),
                values: Ok(vec![("BK000".into(), 5), ("BK001".into(), 7)]),
            },
            cx,
        )
    });
    cx.run_until_parked();
    for key in ["d", "r", "o"] {
        cx.simulate_keystrokes(key);
        cx.run_until_parked();
        assert_eq!(
            dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
            Some("not a verb while picking values — escape first"),
            "{key} inside the Values stage"
        );
        assert!(
            matches!(
                dialog_state(&shell, &cx, |s| s.stage.clone()),
                objectdialog::Stage::Values { .. }
            ),
            "{key} must not leave the stage"
        );
        assert_eq!(
            dialog_state(&shell, &cx, |s| s.confirm),
            None,
            "{key} must not arm a confirm"
        );
    }
}

/// Space on an already-selected scope dimension explains that Enter opens its Values
/// stage, without issuing a data request.
#[gpui::test]
fn space_on_a_selected_scopes_dimension_names_the_values_door(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    let requested = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    cx.update(|_, cx| {
        let requested = requested.clone();
        cx.subscribe(&shell, move |_, e: &ShellEvent, _| {
            if let ShellEvent::DistinctRequested(p) = e {
                requested.borrow_mut().push(p.clone());
            }
        })
        .detach();
    });
    cx.simulate_keystrokes("enter");
    // The stage opens on book, skipping the inert Dimensions header.
    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some("enter opens this dimension's values")
    );
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit { .. }
    ));
    assert!(
        requested.borrow().is_empty(),
        "space must not ask the data for anything"
    );
}

/// Removing a selected scope dimension drops its saved selection entirely, moves the
/// row to available dimensions, and clears its value-summary note. It must not leave an
/// empty selection or a stale summary.
#[gpui::test]
fn x_on_a_selected_scopes_dimension_removes_it_and_clears_its_note(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    cx.simulate_keystrokes("enter");
    // The stage opens on book, skipping the inert Dimensions header.
    cx.simulate_keystrokes("x");
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(400));
    cx.run_until_parked();
    assert_eq!(
        saved_scope_books(&shell, &cx),
        None,
        "a dropped selection is removed, never written as []"
    );
    assert!(
        edit_draft(&shell, &cx, |d| d
            .available_items("dimensions")
            .unwrap()
            .iter()
            .any(|i| i.name == "book" && i.note.is_none())),
        "book moved to the available block with no leftover note"
    );
}

/// `x` on an AVAILABLE Scopes row — one with nothing selected to drop —
/// names `enter`, the door that actually picks its values, rather than
/// `Draft::remove_selected`'s generic "not in the view" wording.
#[gpui::test]
fn x_on_an_available_scopes_row_is_refused(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope_and_an_available_dimension(),
        dir.path(),
        "config::scopes",
    );
    cx.simulate_keystrokes("enter");
    // The stage opens on book, skipping the Dimensions header.
    cx.simulate_keystrokes("j"); // `lhu`, the available row
    cx.simulate_keystrokes("x");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some("not selected — enter picks its values")
    );
}

/// Grouping slots form a fixed list of nine. `n` explains that no additional slot can
/// be created.
#[gpui::test]
fn n_is_inert_on_groupings_and_says_why(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_slot_3(&["book"]),
        dir.path(),
        "config::groupings",
    );
    cx.simulate_keystrokes("n");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    );
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(notice.contains("slots"), "{notice}");
}

// Groupings: digit navigation and chain editing.

/// A bare digit in the Groupings browse list opens that slot's edit
/// stage in one keystroke — the slots are numbered, and the number is
/// the fastest way to name one. On every other domain the digit is
/// claimed and dropped like any other key browse has no verb for, so a
/// `3` typed at the Views list neither opens anything nor leaks to the
/// shell as `ctrl+3`'s bare cousin.
#[gpui::test]
fn a_digit_in_browse_opens_that_slot_on_groupings_only(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_slot_3(&["book"]),
        dir.path(),
        "config::groupings",
    );
    cx.simulate_keystrokes("3");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "3".to_string()
        }
    );
    assert_eq!(
        edit_draft(&shell, &cx, |d| d
            .list_items("dimensions")
            .unwrap()
            .iter()
            .filter(|i| i.included)
            .map(|i| i.name.clone())
            .collect::<Vec<_>>()),
        vec!["book".to_string()],
        "the slot opened is the one the digit named"
    );
    let crumb = shell.read_with(&cx, |shell, _| objectdialog::render::crumb_text(shell));
    assert_eq!(crumb, "ctrl+3");
}

/// The same digit typed at the Views list does nothing at all.
#[gpui::test]
fn a_digit_in_browse_is_dropped_on_a_domain_without_numbered_objects(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_views(), dir.path(), "config::views");
    cx.simulate_keystrokes("3");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    );
    assert!(
        shell.read_with(&cx, |s, _| s.object_dialog.is_some()),
        "still open"
    );
}

/// From one slot's edit stage a digit jumps straight to another's —
/// no `escape`, no re-selection — and the same digit as the open slot
/// says so rather than rebuilding the stage under the trader.
#[gpui::test]
fn a_digit_in_the_edit_stage_jumps_to_that_slot(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_slot_3(&["book"]),
        dir.path(),
        "config::groupings",
    );
    // Row 1 is selected on open.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "1".to_string()
        }
    );
    cx.simulate_keystrokes("3");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "3".to_string()
        }
    );
    assert!(edit_draft(&shell, &cx, |d| d
        .list_items("dimensions")
        .unwrap()[0]
        .included));

    cx.simulate_keystrokes("3");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "3".to_string()
        }
    );
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(notice.contains("already"), "{notice}");
}

/// Reentering a grouping slot within the write debounce rebuilds its draft from
/// pending-aware config. A queued tick must survive both digit jumps and
/// leaving/reentering the edit stage, rather than being erased by a stale draft's next
/// write.
#[gpui::test]
fn jumping_away_and_back_inside_the_debounce_keeps_the_queued_tick(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_slot_3(&["book"]),
        dir.path(),
        "config::groupings",
    );
    // Slot 1 is empty, so ticking book queues a user-layer write without fork
    // confirmation. Opening selects its first dimension candidate, ready for space.
    cx.simulate_keystrokes("1 space");
    cx.run_until_parked();
    assert!(shell.read_with(&cx, |s, _| s.pending_config_write.is_some()));

    cx.simulate_keystrokes("2 1");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "1".to_string()
        }
    );
    let ticked = |shell: &Entity<ShellView>, cx: &gpui::VisualTestContext| {
        edit_draft(shell, cx, |d| {
            d.list_items("dimensions")
                .unwrap()
                .iter()
                .filter(|i| i.included)
                .map(|i| i.name.clone())
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(
        ticked(&shell, &cx),
        vec!["book".to_string()],
        "the queued tick is on screen, not a debounce behind"
    );

    // After accepting the stale-draft overwrite, ticking lhu must retain book. The
    // reopened stage selects book; one j reaches lhu.
    cx.simulate_keystrokes("j space");
    cx.run_until_parked();
    assert_eq!(
        ticked(&shell, &cx),
        vec!["book".to_string(), "lhu".to_string()]
    );
    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("groupings.toml")).unwrap();
    assert!(
        written.contains("1 = [\"book\", \"lhu\"]"),
        "both ticks reach the file: {written}"
    );
}

/// Opening a grouping slot by digit, Enter, or click lands in its normal-mode chooser.
/// `i` opens a seeded chain field with completions; Escape returns through chooser and
/// browse one step at a time.
#[gpui::test]
fn opening_a_slot_lands_in_the_chooser_and_i_opens_the_chain_field(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_slot_3(&["book"]),
        dir.path(),
        "config::groupings",
    );
    // By digit.
    cx.simulate_keystrokes("3");
    cx.run_until_parked();
    assert!(
        !edit_draft(&shell, &cx, |d| d.chain_entry()),
        "the chooser, not the chain field"
    );
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
    assert!(!dialog_filter_is_focused(&shell, &mut cx));
    assert_eq!(dialog_input_text(&shell, &cx), "");
    assert!(cx.debug_bounds("dialog-mode-pill-normal").is_some());
    assert!(cx.debug_bounds("dialog-mode-pill-chain").is_none());
    assert!(cx.debug_bounds("objectdialog-actions").is_some());
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "3".to_string()
        }
    );

    // `i` opens the field, seeded with the chain and holding the keys;
    // a digit typed there is text, not a jump.
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    assert!(
        edit_draft(&shell, &cx, |d| d.chain_entry()),
        "the field is open"
    );
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Filter);
    assert!(dialog_filter_is_focused(&shell, &mut cx));
    assert_eq!(dialog_input_text(&shell, &cx), "book");
    assert!(cx.debug_bounds("dialog-mode-pill-chain").is_some());
    assert!(cx.debug_bounds("objectdialog-actions").is_none());
    cx.simulate_input(" 4");
    cx.run_until_parked();
    assert_eq!(dialog_input_text(&shell, &cx), "book 4");
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "3".to_string()
        }
    );

    // First escape: back to the chooser, with the chain untouched.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!edit_draft(&shell, &cx, |d| d.chain_entry()));
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "3".to_string()
        }
    );
    assert!(cx.debug_bounds("objectdialog-actions").is_some());
    assert!(!edit_draft(&shell, &cx, |d| d.is_dirty()));

    // `i` reopens it; two escapes from there go field → chooser → browse.
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.chain_entry()));
    cx.simulate_keystrokes("escape escape");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    );

    // By `enter` from the list, the same landing.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(!edit_draft(&shell, &cx, |d| d.chain_entry()));
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
    assert!(!dialog_filter_is_focused(&shell, &mut cx));
}

/// The text the shared dialog `Input` currently holds.
fn dialog_input_text(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> String {
    shell.read_with(cx, |shell, cx| {
        shell.dialog_input.read(cx).text().to_string()
    })
}

/// The chain field is seeded and focused on `i`; its rows show completions, Tab accepts
/// one, and Enter applies the typed chain through the same debounced batch as a tick
/// change.
#[gpui::test]
fn i_opens_the_chain_field_tab_completes_and_enter_writes_the_chain(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_slot_3(&["book"]),
        dir.path(),
        "config::groupings",
    );
    cx.simulate_keystrokes("3 i");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.chain_entry()));
    assert!(
        dialog_filter_is_focused(&shell, &mut cx),
        "the field has the keys"
    );
    assert_eq!(
        dialog_input_text(&shell, &cx),
        "book",
        "seeded with the chain"
    );
    assert!(
        cx.debug_bounds("dialog-name-row").is_some(),
        "the chain field, not the filter row"
    );
    assert!(
        cx.debug_bounds("dialog-mode-pill-chain").is_some(),
        "{:?}",
        "the pill says chain"
    );
    assert!(cx.debug_bounds("dialog-mode-pill-filter").is_none());
    // Hide destructive mouse actions while a value field is open so a click cannot arm
    // confirmation over the focused editor.
    assert!(
        cx.debug_bounds("objectdialog-actions").is_none(),
        "the action bar is withdrawn while the chain field is open"
    );

    cx.simulate_input(" l");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-item-lhu").is_some(),
        "the completion for `l`"
    );
    assert!(
        cx.debug_bounds("objectdialog-item-book").is_none(),
        "already typed, so no longer offered"
    );
    assert!(
        cx.debug_bounds("objectdialog-field-slot").is_none(),
        "no field rows while completing"
    );

    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert_eq!(dialog_input_text(&shell, &cx), "book / lhu / ");

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(!edit_draft(&shell, &cx, |d| d.chain_entry()));
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
    assert!(!dialog_filter_is_focused(&shell, &mut cx));
    assert_eq!(dialog_input_text(&shell, &cx), "");
    assert_eq!(
        edit_draft(&shell, &cx, |d| d
            .list_items("dimensions")
            .unwrap()
            .iter()
            .filter(|i| i.included)
            .map(|i| i.name.clone())
            .collect::<Vec<_>>()),
        vec!["book".to_string(), "lhu".to_string()]
    );
    assert!(
        cx.debug_bounds("objectdialog-field-slot").is_some(),
        "the field rows are back"
    );
    // Slot 3 is the builtin layer's, so the applied chain is a
    // definitional change to an object the user does not own: the same
    // fork a tick or a `shift+j` on it makes
    // (`reordering_slot_3_and_pressing_ctrl_3_regroups_off_the_new_order`),
    // applied and announced the same way.
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_none(),
        "a desk slot forks without asking, from the chain field too"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.pending_config_write.is_some()),
        "queued on the keystroke, like a tick"
    );
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(notice.contains("copied '3'"), "{notice}");
    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("groupings.toml")).unwrap();
    assert!(written.contains("\"book\", \"lhu\""), "{written}");
}

/// A chain the adapter refuses leaves the field open with the text as
/// typed and says what is wrong; `escape` then drops the text and
/// closes the field with the chain untouched and nothing queued.
#[gpui::test]
fn a_refused_chain_keeps_the_field_open_and_escape_cancels_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_slot_3(&["book"]),
        dir.path(),
        "config::groupings",
    );
    cx.simulate_keystrokes("3 i");
    cx.simulate_input(" npv");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(notice.contains("npv"), "{notice}");
    assert!(edit_draft(&shell, &cx, |d| d.chain_entry()), "still open");
    assert!(dialog_filter_is_focused(&shell, &mut cx));
    assert_eq!(
        dialog_input_text(&shell, &cx),
        "book npv",
        "the text is intact"
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!edit_draft(&shell, &cx, |d| d.chain_entry()));
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "3".to_string()
        },
        "escape closed the field, not the stage"
    );
    assert_eq!(dialog_input_text(&shell, &cx), "");
    assert!(!edit_draft(&shell, &cx, |d| d.is_dirty()));
    assert!(shell.read_with(&cx, |s, _| s.pending_config_write.is_none()));
}

/// `i` is the chain field's key on Groupings alone. On Views it keeps
/// the read-only notice `enter` gives — the edit stage has no text
/// field there to open.
#[gpui::test]
fn i_on_views_still_gives_the_read_only_notice(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    assert!(!edit_draft(&shell, &cx, |d| d.chain_entry()));
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
    assert!(dialog_state(&shell, &cx, |s| s.notice.is_some()));
}

// Filtering the edit stage.

/// `/` filters the edit stage's own rows, exactly as it does in browse:
/// the field labels are ranked against the query, a hidden row's element
/// does not paint, `enter` leaves filter mode keeping the query and then
/// `escape` walks the rest of the ladder one visible rung at a time
/// (clear the query, back a stage), and every verb along the way still
/// acts on the row the trader is actually looking at.
#[gpui::test]
fn slash_filters_the_edit_stage_and_escape_walks_the_full_ladder(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());

    cx.simulate_keystrokes("/");
    cx.run_until_parked();
    assert!(dialog_filter_is_focused(&shell, &mut cx));

    cx.simulate_input("npv");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-item-npv").is_some());
    assert!(
        cx.debug_bounds("objectdialog-item-book").is_none(),
        "hidden by the filter"
    );
    assert!(cx.debug_bounds("objectdialog-field-dataset").is_none());

    // Leave filter mode with Enter, retaining the query.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
    assert!(
        cx.debug_bounds("objectdialog-item-book").is_none(),
        "query still applied"
    );

    // `space` acts on npv, the filtered row — not on whatever unfiltered
    // row happens to sit at index 0.
    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| !d
        .list_items("columns")
        .unwrap()[1]
        .included));

    // Clear the query.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-item-book").is_some());

    // Back a stage.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    );
    // Edit-stage filtering updates the draft's query without changing the independent
    // browse query.
    assert!(
        dialog_state(&shell, &cx, |s| s.query.clone()).is_empty(),
        "the browse query must not carry the edit stage's filter"
    );
    shell.read_with(&cx, |shell, _| {
        let state = shell
            .object_dialog
            .as_ref()
            .expect("the object dialog should be open");
        let rows = state.domain.objects(&shell.services.config);
        let visible = objectdialog::visible_rows(state, &rows);
        assert_eq!(
            visible.len(),
            rows.len(),
            "an empty browse query must show every row — this fixture has \
             only `tree`, so a leaked filter would empty the list rather \
             than merely narrow it"
        );
        let selected_name = visible
            .get(state.selected)
            .and_then(|m| rows.get(m.row))
            .map(|r| r.name.as_str());
        assert_eq!(
            selected_name,
            Some("tree"),
            "the cursor is back on the object just edited"
        );
    });
}

/// Escape restores the edit stage's own query and leaves that stage open.
/// The browse query is separate state and must remain unchanged.
#[gpui::test]
fn escape_reverts_the_edit_stages_own_query(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());

    cx.simulate_keystrokes("/");
    cx.simulate_input("npv");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        edit_draft(&shell, &cx, |d| d.query == "npv"),
        "sanity: enter kept the edit stage's filter"
    );

    cx.simulate_keystrokes("/");
    cx.simulate_input("zzz");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
    assert!(
        edit_draft(&shell, &cx, |d| d.query == "npv"),
        "escape puts back the query the second filter session started from"
    );
    assert!(
        edit_draft(&shell, &cx, |d| d.selected == 0),
        "with the cursor on the top match of the list that came back"
    );
    assert_eq!(
        shell.read_with(&cx, |s, cx| s.dialog_input.read(cx).value().to_string()),
        "npv",
        "and the field follows the restored draft query"
    );
    assert!(
        matches!(
            dialog_state(&shell, &cx, |s| s.stage.clone()),
            objectdialog::Stage::Edit { .. }
        ),
        "reverting a search never leaves the stage"
    );
    assert!(
        dialog_state(&shell, &cx, |s| s.query.clone()).is_empty(),
        "and never touches the browse query's own slot"
    );
}

/// Clicking an available Views candidate while filtering preserves Filter mode and
/// input focus. The row must accept selection without opening a nested stage: space can
/// add it, but it has neither a column nor a Values-stage target. An inert row's
/// rejected click would not exercise focus synchronization, and a member row would
/// deliberately enter a new stage in Normal mode.
#[gpui::test]
fn clicking_an_edit_row_while_filtering_keeps_the_filter_focused(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    cx.simulate_keystrokes("/");
    cx.run_until_parked();
    let before = edit_draft(&shell, &cx, |d| d.selected);
    assert_eq!(
        cursor_item_name(&shell, &cx).as_deref(),
        Some("book"),
        "the stage opens on its first member, so the click below is a real move"
    );
    let row = cx
        .debug_bounds("objectdialog-item-delta01")
        .expect("the catalogue's own row should paint");
    // Click near the row's top edge. Its section header shares the row element, so the
    // box can extend below the visible viewport and its center may be outside the list.
    cx.simulate_click(
        gpui::point(row.origin.x + gpui::px(8.0), row.origin.y + gpui::px(2.0)),
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();

    assert_ne!(
        edit_draft(&shell, &cx, |d| d.selected),
        before,
        "the click moved the cursor onto the row it landed on"
    );
    assert_eq!(
        cursor_item_name(&shell, &cx).as_deref(),
        Some("delta01"),
        "which is the catalogue row"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "tree".to_string()
        },
        "a catalogue row opens nothing"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.mode),
        DialogMode::Filter,
        "a click must not change which mode the stage is in"
    );
    assert!(
        dialog_filter_is_focused(&shell, &mut cx),
        "and filter mode's own surface must still hold the keyboard"
    );
    cx.simulate_input("p");
    cx.run_until_parked();
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.query.clone()),
        "p",
        "so the next character typed still reaches the filter"
    );
}

/// Column stages offer no Delete, Revert, or overwrite buttons on either the Views or
/// Schema path. Give the fixture a real user override so the destructive buttons would
/// otherwise be enabled.
#[gpui::test]
fn a_column_stage_offers_no_destructive_action(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = open_views_dialog(cx);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-action-d").is_some(),
        "sanity: tree has a user-layer copy, so the edit stage offers d"
    );
    assert!(
        cx.debug_bounds("objectdialog-action-r").is_some(),
        "sanity: and it overrides a builtin, so the edit stage offers r"
    );
    // Dataset row, Columns row, then the one member — `enter` on it.
    cx.simulate_keystrokes("j j enter");
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Column { .. }
    ));
    assert!(cx.debug_bounds("objectdialog-action-d").is_none());
    assert!(cx.debug_bounds("objectdialog-action-r").is_none());
    // And back out again, so the gate is the stage and not a one-way
    // door.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-action-d").is_some());
}

/// Clicking a view member row opens that column's stage, matching Enter.
#[gpui::test]
fn clicking_a_member_row_opens_its_column_stage(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    let row = cx
        .debug_bounds("objectdialog-item-npv")
        .expect("npv is one of tree's own columns");
    cx.simulate_click(
        gpui::point(row.origin.x + gpui::px(8.0), row.origin.y + gpui::px(2.0)),
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Column {
            object: "tree".to_string(),
            column: "npv".to_string()
        },
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| objectdialog::render::crumb_text(s)),
        "tree › npv"
    );
}

/// The grip is the member row's drag handle: a press on it arms the drag
/// and nothing else, so a column can be reordered by the mouse without
/// the press opening that column's stage the way a press on the row's
/// body does.
#[gpui::test]
fn pressing_a_member_rows_grip_does_not_open_its_column_stage(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    let grip = cx
        .debug_bounds("objectdialog-grip-npv")
        .expect("npv is one of tree's own columns and paints a grip");
    cx.simulate_mouse_down(
        gpui::point(grip.origin.x + gpui::px(3.0), grip.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "tree".to_string()
        },
        "a press on the grip leaves the dialog on the view's edit stage"
    );
}

// Object-dialog breadcrumbs, badges, row controls, and section headers.

#[gpui::test]
fn the_edit_stage_paints_section_headers_and_the_crumb_and_no_destination_badge(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (_shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    assert!(
        cx.debug_bounds("objectdialog-section-members-columns")
            .is_some()
    );
    assert!(
        cx.debug_bounds("objectdialog-section-available-columns")
            .is_some(),
        "delta01 is available"
    );
    // Every field here writes the view document, so a per-row `doc` badge
    // would say the same thing on every row.
    assert!(cx.debug_bounds("objectdialog-dest-dataset").is_none());
    assert!(cx.debug_bounds("objectdialog-dest-columns").is_none());
    assert!(cx.debug_bounds("dialog-mode-pill-normal").is_some());
}

#[gpui::test]
fn the_browse_crumb_counts_and_a_slot_crumb_names_its_chord(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_slot_3(&["book"]),
        dir.path(),
        "config::groupings",
    );
    let crumb = shell.read_with(&cx, |shell, _| objectdialog::render::crumb_text(shell));
    assert_eq!(crumb, "9 slots");
    cx.simulate_keystrokes("j j enter");
    cx.run_until_parked();
    let crumb = shell.read_with(&cx, |shell, _| objectdialog::render::crumb_text(shell));
    assert_eq!(crumb, "ctrl+3");
}

/// A desk view whose dataset has enough columns that its edit stage
/// overflows `VISIBLE_ROWS` — `book`/`npv` as `tree`'s two members, plus
/// fourteen more measures (`m0`..`m13`) it has not picked up, so both
/// section headers paint (members, then available) and reaching the
/// bottom of the list needs an actual scroll, not just more rows fitting
/// on screen.
fn services_with_a_long_desk_view() -> ShellServices {
    long_desk_view_services(14)
}

/// That fixture with `extra` unpicked measures instead of fourteen.
///
/// Fourteen is enough to make the list scroll at all, which is all the
/// section-header test needs; it is *not* enough to put the top of the
/// list off screen while the bottom is showing (the viewport holds
/// roughly seventeen rows), so a verb that moves a row from the bottom
/// of the list to the member block at the top still lands it in view by
/// accident. The cursor-in-view tests for `space`/`shift+space` pass a
/// much larger count for exactly that reason.
fn long_desk_view_services(extra: usize) -> ShellServices {
    let mut columns = String::from(
        "[risk_snapshot.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk_snapshot.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
    );
    for i in 0..extra {
        columns.push_str(&format!(
            "[risk_snapshot.columns.m{i}]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n"
        ));
    }
    let datasets = LayerDoc::builtin("datasets", &columns).unwrap();
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
    let mut services = test_services();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![datasets, desk],
        desk: None,
        user: None,
    });
    services
}

/// The structural property the mutation entry `objectdialog: section
/// headers do not add list children` guards: with the section headers
/// folded into their block's first item (rather than each being its own
/// `list.child`), the edit list's child count equals its visible-row
/// count, so `scroll_to_item(draft.selected)` — driven here by `shift-g`
/// (`vimnav::NavCommand::Bottom`) — always targets the right child. If a
/// header were instead a separate child, every row from the first header
/// onward would be one child index further than `selected` expects, and
/// `shift-g` would leave the true last row (`m13`, the last available
/// column) short of the bottom of the scrolled viewport instead of
/// flush with it.
#[gpui::test]
fn the_cursor_stays_in_view_past_a_section_header_on_a_long_list(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_long_desk_view(),
        dir.path(),
        "config::views",
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("shift-g");
    cx.run_until_parked();

    let list = cx
        .debug_bounds("objectdialog-fields")
        .expect("the fields list should paint");
    // `m13` is the dataset's last column and `tree` never picked it up,
    // so it is the last row of the available block — and so the last row
    // of the whole list, which is exactly what `shift-g` should scroll
    // to the bottom of.
    let row = cx
        .debug_bounds("objectdialog-item-m13")
        .expect("the last available column should paint");
    assert!(
        row.origin.y + gpui::px(1.0) >= list.origin.y,
        "the last row's top ({:?}) should not sit above the list's own top ({:?})",
        row.origin.y,
        list.origin.y
    );
    assert!(
        row.origin.y + row.size.height <= list.origin.y + list.size.height + gpui::px(1.0),
        "the last row (bottom {:?}) should be fully inside the list's \
         viewport (bottom {:?}), not scrolled past it",
        row.origin.y + row.size.height,
        list.origin.y + list.size.height
    );
}

/// `selector`'s row lies wholly inside the edit list's own viewport —
/// the shape of "the cursor is still on screen" for a verb that moved
/// the row it was on. A 1px margin either way, since the row's box and
/// the scrolled viewport can share an edge.
fn assert_row_in_view(cx: &mut gpui::VisualTestContext, selector: &'static str, after: &str) {
    let list = cx
        .debug_bounds("objectdialog-fields")
        .expect("the fields list should paint");
    let row = cx
        .debug_bounds(selector)
        .expect("the moved row should paint");
    assert!(
        row.origin.y + gpui::px(1.0) >= list.origin.y,
        "after {after} the row's top ({:?}) is above the list's own top \
         ({:?}) — the cursor moved off the top of the viewport",
        row.origin.y,
        list.origin.y
    );
    assert!(
        row.origin.y + row.size.height <= list.origin.y + list.size.height + gpui::px(1.0),
        "after {after} the row's bottom ({:?}) is past the list's own \
         bottom ({:?}) — the cursor moved off the end of the viewport",
        row.origin.y + row.size.height,
        list.origin.y + list.size.height
    );
}

/// Name of the edit cursor's list item, whether a member or an available candidate;
/// field rows return `None`.
fn cursor_item_name(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> Option<String> {
    edit_draft(shell, cx, |draft| match draft.selected_row()? {
        objectdialog::EditRow::Item { field, item } => draft
            .list_items(&draft.fields[field].key)
            .map(|items| items[item].name.clone()),
        objectdialog::EditRow::Available { field, item } => draft
            .available_items(&draft.fields[field].key)
            .map(|items| items[item].name.clone()),
        objectdialog::EditRow::Field(_) => None,
    })
}

/// Groupings opens on a dimension row and navigation skips the inert Slot and
/// Dimensions fields in either direction. The rows remain visible.
#[gpui::test]
fn the_slot_and_dimensions_rows_are_not_cursor_stops(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = services_with_slot_3(&["book", "lhu"]);
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::groupings");
    cx.simulate_keystrokes("j j enter"); // slot 3's edit stage
    cx.run_until_parked();

    assert!(
        edit_draft(&shell, &cx, |d| d.rows().len() >= 4),
        "sanity: the slot has its two field rows and at least two dimensions"
    );
    assert_eq!(
        cursor_item_name(&shell, &cx).as_deref(),
        Some("book"),
        "the stage opens on the chain's first dimension, not on the Slot row"
    );
    assert!(
        cx.debug_bounds("objectdialog-field-slot").is_some(),
        "the Slot row still PAINTS — it is the slot's number, just not a cursor stop"
    );

    // Backward motion from the first stop wraps to the last dimension, skipping both
    // inert field rows.
    cx.simulate_keystrokes("k");
    cx.run_until_parked();
    assert!(
        cursor_item_name(&shell, &cx).is_some(),
        "k wrapped onto a dimension row, not onto a field row"
    );
    cx.simulate_keystrokes("j");
    cx.run_until_parked();
    assert_eq!(
        cursor_item_name(&shell, &cx).as_deref(),
        Some("book"),
        "and j comes back round to the first dimension"
    );

    // Every cursor stop in this fixture is a dimension row.
    for _ in 0..8 {
        cx.simulate_keystrokes("j");
        cx.run_until_parked();
        assert!(
            cursor_item_name(&shell, &cx).is_some(),
            "every stop on the way round is a dimension row, never Slot or Dimensions"
        );
    }
}

/// Clicking the inert Slot row leaves selection unchanged.
#[gpui::test]
fn a_click_on_the_slot_row_is_dropped(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = services_with_slot_3(&["book", "lhu"]);
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::groupings");
    cx.simulate_keystrokes("j j enter");
    cx.run_until_parked();
    let before = edit_draft(&shell, &cx, |d| d.selected);

    let row = cx
        .debug_bounds("objectdialog-field-slot")
        .expect("the Slot row paints");
    cx.simulate_click(
        gpui::point(row.origin.x + gpui::px(20.0), row.origin.y + gpui::px(2.0)),
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.selected),
        before,
        "the click left the cursor on the row it was already on"
    );
    assert_eq!(
        cursor_item_name(&shell, &cx).as_deref(),
        Some("book"),
        "which is still a dimension row"
    );
}

/// Query mirroring resets selection to the first ranked match, then settles onto a stop
/// when one exists. Here l ranks the inert Columns header before the available delta01
/// row, so selection must advance to delta01.
#[gpui::test]
fn a_filter_keystroke_never_leaves_the_cursor_on_a_header(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    cx.simulate_keystrokes("/");
    cx.simulate_input("l");
    cx.run_until_parked();

    let top = edit_draft(&shell, &cx, |d| {
        let rows = d.rows();
        d.visible_rows()
            .first()
            .and_then(|m| rows.get(m.row).copied())
    });
    assert_eq!(
        top,
        Some(objectdialog::EditRow::Field(1)),
        "sanity: the list header ranks first under this query, so the mirror's own \
         reset would land the cursor on it"
    );
    assert!(
        cursor_item_name(&shell, &cx).is_some(),
        "the cursor settled past it onto a row that answers to something"
    );
}

/// Promoting an available column keeps the cursor on the next available row. If that
/// row falls below the viewport, scroll it into view so the next navigation command
/// begins from a visible selection.
#[gpui::test]
fn space_scrolls_the_next_row_into_view(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, long_desk_view_services(40), dir.path(), "config::views");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let last = cursor_to_last_visible_available_row(&shell, &mut cx);
    let next = format!("m{}", last + 1);
    let next_selector = item_selector(last + 1);
    assert!(
        !row_in_view(&mut cx, next_selector),
        "sanity: {next} starts just below the viewport"
    );
    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    assert_eq!(
        cursor_item_name(&shell, &cx).as_deref(),
        Some(next.as_str()),
        "the cursor moved on to the next available column, not with m{last}"
    );
    assert!(
        row_in_view(&mut cx, next_selector),
        "after space the cursor row {next} was not scrolled into view"
    );
}

/// The debug selector of available column `m{i}`, leaked to the
/// `&'static str` `debug_bounds` insists on.
fn item_selector(i: usize) -> &'static str {
    Box::leak(format!("objectdialog-item-m{i}").into_boxed_str())
}

/// Is `selector`'s row wholly inside the edit list's viewport? The
/// predicate behind [`assert_row_in_view`], for a test that has to
/// *find* the viewport's last row before it can assert on the next one.
fn row_in_view(cx: &mut gpui::VisualTestContext, selector: &'static str) -> bool {
    let list = cx
        .debug_bounds("objectdialog-fields")
        .expect("the fields list should paint");
    let Some(row) = cx.debug_bounds(selector) else {
        return false;
    };
    row.origin.y + gpui::px(1.0) >= list.origin.y
        && row.origin.y + row.size.height <= list.origin.y + list.size.height + gpui::px(1.0)
}

/// Walk the cursor from the top of a fresh `long_desk_view_services(40)`
/// edit stage down to `m{n}`, the LAST available column the unscrolled
/// viewport shows in full, and return `n`. Read off the painted layout
/// rather than hardcoded, because how many one-line rows fit under the
/// `VISIBLE_ROWS * ROW_HEIGHT` cap (a two-line estimate) is a layout
/// fact this test has no business restating — an earlier draft assumed
/// ten and left the harness's scroll entries surviving, since the next
/// row was already on screen.
fn cursor_to_last_visible_available_row(
    shell: &Entity<ShellView>,
    cx: &mut gpui::VisualTestContext,
) -> usize {
    // `debug_bounds` wants a `&'static str`; a leaked selector per probed
    // row is the price, and a test's to pay.
    let last = (0..40)
        .take_while(|i| row_in_view(cx, item_selector(*i)))
        .last()
        .expect("at least m0 is in view at the top");
    assert!(
        last < 39,
        "the fixture must be taller than the viewport for the test to mean anything"
    );
    // Rows are Dataset=0, Columns=1, book=2, npv=3, m0=4, and so on. Opening selects
    // book, so m{last} is last + 2 downward steps away.
    let presses = vec!["j"; last + 2].join(" ");
    cx.simulate_keystrokes(&presses);
    cx.run_until_parked();
    assert_eq!(
        cursor_item_name(shell, cx).as_deref(),
        Some(format!("m{last}").as_str())
    );
    last
}

/// `shift+space` steps the same row the same way (both directions share
/// `Draft::step_selected`, whose add branch has no direction of its
/// own), so it has to scroll for the same reason — asserted separately
/// because the two arms are separate call sites in `handle_key`.
#[gpui::test]
fn shift_space_scrolls_the_next_row_into_view(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, long_desk_view_services(40), dir.path(), "config::views");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let last = cursor_to_last_visible_available_row(&shell, &mut cx);
    let next = format!("m{}", last + 1);
    let next_selector = item_selector(last + 1);
    assert!(!row_in_view(&mut cx, next_selector));
    cx.simulate_keystrokes("shift-space");
    cx.run_until_parked();
    assert_eq!(
        cursor_item_name(&shell, &cx).as_deref(),
        Some(next.as_str())
    );
    assert!(
        row_in_view(&mut cx, next_selector),
        "after shift+space the cursor row {next} was not scrolled into view"
    );
}

/// Adding the LAST available row has no next row to move on to, so the
/// cursor stays at the same visible index — now the row before it, `m38`
/// — rather than following `m39` up to the member block a screenful
/// above or running off the end of the list.
#[gpui::test]
fn space_on_the_last_available_row_keeps_the_cursor_at_the_bottom(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, long_desk_view_services(40), dir.path(), "config::views");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("shift-g");
    cx.run_until_parked();
    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    assert_eq!(cursor_item_name(&shell, &cx).as_deref(), Some("m38"));
    assert_row_in_view(&mut cx, "objectdialog-item-m38", "space");
}

/// Removing a member moves it to the end of the available block, while the cursor
/// remains on the following row at its existing visible index. The viewport must not
/// follow the removed item.
#[gpui::test]
fn x_leaves_the_cursor_on_the_next_row_still_in_view(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_long_desk_view(),
        dir.path(),
        "config::views",
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    // The stage opens on book, skipping the inert Dataset choice and Columns header.
    assert_eq!(cursor_item_name(&shell, &cx).as_deref(), Some("book"));
    cx.simulate_keystrokes("x");
    cx.run_until_parked();
    assert_eq!(
        cursor_item_name(&shell, &cx).as_deref(),
        Some("npv"),
        "the cursor stayed on the next member, not with book at the bottom"
    );
    assert_row_in_view(&mut cx, "objectdialog-item-npv", "x");
    assert!(
        !row_in_view(&mut cx, "objectdialog-item-book"),
        "sanity: book really did travel off the bottom of the viewport"
    );
}

// ---------------------------------------------------------------------
// Height bug: the edit list's fixed height did not count the section
// headers folded into each block's first item, so a list short enough
// not to hit the `VISIBLE_ROWS` cap painted too short a container and
// clipped its last row.
// ---------------------------------------------------------------------

/// A filtered edit stage down to one available row — the reported
/// screenshot's own case (Views, filter `ex`, one row: `expiry`). Here:
/// `tree`'s edit stage, filtered to `delta`, leaves `delta01` as the
/// only visible row, under one "available" section header. The
/// container has to be tall enough for header-plus-row, not just one
/// `FIELD_ROW_HEIGHT`.
#[gpui::test]
fn a_filtered_single_row_is_not_clipped_by_the_lists_height(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());

    cx.simulate_keystrokes("/");
    cx.run_until_parked();
    assert!(dialog_filter_is_focused(&shell, &mut cx));

    cx.simulate_input("delta");
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("objectdialog-section-available-columns")
            .is_some(),
        "the available section header should still paint"
    );
    assert_row_in_view(&mut cx, "objectdialog-item-delta01", "filtering to one row");
}

/// The unfiltered short-list case: a Groupings slot with three
/// dimensions (`book`, `lhu`, `position_ref`) is well under the
/// `VISIBLE_ROWS` cap, so the container's height comes entirely from the
/// estimate rather than the cap — and the estimate did not count the
/// one "dimensions" section header folded into the first item, so the
/// last member (`position_ref`) painted clipped.
#[gpui::test]
fn an_unfiltered_short_list_is_not_clipped_by_the_lists_height(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_slot_3(&["book", "lhu", "position_ref"]),
        dir.path(),
        "config::groupings",
    );
    // Row 1 is selected on open; navigate down to slot 3, then open it.
    cx.simulate_keystrokes("j j");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert_row_in_view(
        &mut cx,
        "objectdialog-item-position_ref",
        "opening a short, unfiltered list",
    );
}

// Confirmation buttons call `run_confirmed` without passing through key handling, so
// they must synchronize text and focus after changing stages.

/// Answering a destructive question **with the mouse** while the edit
/// stage is filtering has to leave the shared field agreeing with the
/// stage it lands in.
///
/// It is the one route to an armed confirm from `DialogMode::Filter` at
/// all: while a confirm is armed `handle_edit_key` swallows every
/// keystroke, `/` included, so the keyboard can only arm one from normal
/// mode — but the action bar's buttons are live in either mode
/// (`press_verb`). Confirming then walks all the way back to browse
/// through `leave_edit`, which clears the query and — deliberately —
/// leaves the mode alone, so the trader is still filtering. Nothing on
/// that path goes near `ShellView::handle_key_down`, so without
/// `dialog::sync_dialog_text` at the end of the button's own closure the
/// browse list would come back unfiltered under a field still showing
/// the edit stage's query: a filter that is painted, focused, and no
/// longer applied to anything.
#[gpui::test]
fn confirming_with_the_mouse_while_filtering_empties_the_field(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_views(), dir.path(), "config::views");
    // `tree` is the first row and the user layer owns it, so its edit
    // stage offers `d`.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("/");
    cx.run_until_parked();
    cx.simulate_input("d");
    cx.run_until_parked();
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.query.clone()),
        "d",
        "the edit stage is filtering by its own query"
    );

    click_selector(&mut cx, "objectdialog-action-d");
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_some(),
        "the mouse armed the delete without leaving filter mode"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.mode),
        DialogMode::Filter,
        "arming with the mouse must not change the mode"
    );

    click_selector(&mut cx, "objectdialog-confirm-yes");
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse,
        "a confirmed delete goes back to the list"
    );
    assert_eq!(
        shell.read_with(&cx, |s, cx| s.dialog_input.read(cx).value().to_string()),
        "",
        "and the field it was filtering with is emptied to match the \
         query `leave_edit` cleared"
    );
    assert!(
        dialog_filter_is_focused(&shell, &mut cx),
        "the mode is still `Filter`, so the filter still owns the keys"
    );
}

/// Click the centre of whatever `selector` painted, and let the frame
/// settle. `debug_bounds` takes a `&'static str`, so the selectors are
/// spelled out at each call site rather than formatted.
fn click_selector(cx: &mut gpui::VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} should have painted"));
    cx.simulate_click(bounds.center(), gpui::Modifiers::default());
    cx.run_until_parked();
}

// Mouse interaction parity.

/// Clicking the browse stage's frozen filter enters filter mode.
#[gpui::test]
fn clicking_the_browse_frozen_filter_row_enters_filter_mode(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = open_views_dialog(cx);
    let row = cx
        .debug_bounds("dialog-filter-frozen")
        .expect("browse opens in normal mode with the row frozen");
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(20.0), row.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Filter);
    cx.simulate_input("w");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.query.clone()), "w");
    // Mouse entry captures the same query snapshot as `/`.
    // The entry query here is empty, so Escape restores an empty filter.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.query.clone()), "");
    assert!(
        shell.read_with(&cx, |s, _| s.modal.is_some()),
        "and the dialog is still open — this was the revert rung, not a close"
    );
}

/// The edit stage's frozen filter enters its own draft filter.
#[gpui::test]
fn clicking_the_edit_frozen_filter_row_enters_filter_mode(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    let row = cx
        .debug_bounds("dialog-filter-frozen")
        .expect("the edit stage opens in normal mode with the row frozen");
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(20.0), row.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Filter);
    cx.simulate_input("n");
    cx.run_until_parked();
    assert_eq!(edit_draft(&shell, &cx, |d| d.query.clone()), "n");
}

/// A browse-row click selects and opens that row through `enter_edit_stage`, matching
/// normal-mode Enter.
#[gpui::test]
fn clicking_a_browse_row_opens_its_edit_stage(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = open_views_dialog(cx);
    let row = cx
        .debug_bounds("objectdialog-row-wide")
        .expect("the wide view paints a browse row");
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(8.0), row.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "wide".to_string()
        },
        "the click opened wide"
    );
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
}

/// Clicking a grouping row opens its chooser without opening the chain field.
#[gpui::test]
fn clicking_a_groupings_row_opens_the_chooser(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_slot_3(&["book", "lhu"]),
        dir.path(),
        "config::groupings",
    );
    let row = cx
        .debug_bounds("objectdialog-row-3")
        .expect("slot 3 paints");
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(8.0), row.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "3".to_string()
        },
        "the click opened slot 3"
    );
    assert!(!edit_draft(&shell, &cx, |d| d.chain_entry()));
    assert_eq!(edit_draft(&shell, &cx, |d| d.query.clone()), "");
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
}

/// While naming, clicking a browse row selects without discarding the typed name or
/// opening an edit stage. The list remains ranked by the naming text so near-collisions
/// stay visible.
///
/// Type `wd`, which matches the fixture's `wide` row but differs from its name. A
/// nonmatching query would leave nothing to click; typing `wide` itself would fail to
/// detect a click that overwrote the query with the row name.
#[gpui::test]
fn clicking_a_browse_row_while_naming_only_selects(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = open_views_dialog(cx);
    cx.simulate_keystrokes("n");
    cx.run_until_parked();
    cx.simulate_input("wd");
    cx.run_until_parked();
    let row = cx
        .debug_bounds("objectdialog-row-wide")
        .expect("a subsequence of an existing row's name keeps that row visible");
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(8.0), row.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Naming
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.query.clone()),
        "wd",
        "the typed name survived, unreplaced by the row it selected"
    );
}

/// Clicking a shown column's tick hides it through a presentation write and leaves the
/// cursor on that row, matching Space without copying the view definition.
#[gpui::test]
fn clicking_a_tick_hides_the_column_and_parks_the_cursor_there(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    let tick = cx
        .debug_bounds("objectdialog-tick-npv")
        .expect("npv paints a tick");
    cx.simulate_mouse_down(
        gpui::point(tick.origin.x + gpui::px(4.0), tick.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    let included = edit_draft(&shell, &cx, |d| {
        d.list_items("columns")
            .unwrap()
            .iter()
            .find(|i| i.name == "npv")
            .unwrap()
            .included
    });
    assert!(!included, "npv is hidden");
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.row_label(d.selected_row().unwrap())),
        "npv"
    );
    // The write is a Presentation one: after the debounce the user
    // layer's view_presentation.toml names npv hidden.
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(400));
    cx.run_until_parked();
    let text =
        std::fs::read_to_string(dir.path().join("view_presentation.toml")).unwrap_or_default();
    assert!(
        text.contains("[tree.columns.npv]") && text.contains("hidden = true"),
        "{text}"
    );
}

/// On an available row the tick adds — `space`'s add — which is a `Doc`
/// write, so on a desk view it forks the view and says so.
#[gpui::test]
fn clicking_an_available_rows_tick_adds_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    let tick = cx
        .debug_bounds("objectdialog-tick-delta01")
        .expect("delta01 is available");
    cx.simulate_mouse_down(
        gpui::point(tick.origin.x + gpui::px(4.0), tick.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    let names: Vec<String> = edit_draft(&shell, &cx, |d| {
        d.list_items("columns")
            .unwrap()
            .iter()
            .map(|i| i.name.clone())
            .collect()
    });
    assert_eq!(names, ["book", "npv", "delta01"]);
    assert!(
        dialog_state(&shell, &cx, |s| s.confirm.is_none()),
        "a desk view forks without asking"
    );
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(notice.contains("copied 'tree'"), "{notice}");
}

/// An armed confirmation consumes tick clicks without changing the draft. Use a
/// user-owned view so Delete can actually arm; a desk-owned view would only show a
/// refusal notice.
#[gpui::test]
fn a_tick_click_does_nothing_while_a_confirm_is_armed(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_user_only_view(),
        dir.path(),
        "config::views",
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.confirm),
        Some(objectdialog::Confirm::Delete),
        "d arms delete on an object the user layer itself defines"
    );

    let selected_before = edit_draft(&shell, &cx, |d| d.selected);

    let tick = cx
        .debug_bounds("objectdialog-tick-npv")
        .expect("npv paints a tick");
    cx.simulate_mouse_down(
        gpui::point(tick.origin.x + gpui::px(4.0), tick.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();

    assert_eq!(
        dialog_state(&shell, &cx, |s| s.confirm),
        Some(objectdialog::Confirm::Delete),
        "the armed delete must not be clobbered by a tick click behind it"
    );
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.selected),
        selected_before,
        "the tick's stop_propagation must keep the click from falling through \
         to on_edit_row_clicked, which would move the cursor to npv's row"
    );
    assert!(
        edit_draft(&shell, &cx, |d| {
            d.list_items("columns")
                .unwrap()
                .iter()
                .find(|i| i.name == "npv")
                .unwrap()
                .included
        }),
        "npv must still be included — the tick click did nothing, not even toggle it"
    );

    cx.executor()
        .advance_clock(std::time::Duration::from_millis(400));
    cx.run_until_parked();
    assert!(
        !dir.path().join("view_presentation.toml").exists(),
        "no write reaches disk from a tick click claimed by an armed confirm"
    );
}

/// The drop handler reorders rows, selects the dropped item, and queues a presentation
/// write. Call the handler directly: the test harness does not activate GPUI's row-drag
/// machinery through simulated mouse events. These assertions cover drop handling; they
/// do not establish the rendered drag-and-drop wiring.
#[gpui::test]
fn the_drop_handler_reorders_and_writes(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    // Put the cursor on the target, distinct from the dragged row, so the test detects
    // a handler that incorrectly derives its source from selection instead of the
    // payload.
    cx.simulate_keystrokes("j");
    cx.run_until_parked();
    let (src, dst) = edit_draft(&shell, &cx, |d| {
        let rows = d.rows();
        let book = rows
            .iter()
            .copied()
            .find(|r| d.row_label(*r) == "book")
            .unwrap();
        let npv = rows
            .iter()
            .copied()
            .find(|r| d.row_label(*r) == "npv")
            .unwrap();
        assert_eq!(d.row_label(d.selected_row().unwrap()), "npv");
        (d.row_drag(book).unwrap(), d.row_drag(npv).unwrap())
    });
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            objectdialog::render::on_row_dropped(shell, &src, &dst, window, cx);
        });
    });
    cx.run_until_parked();
    let names: Vec<String> = edit_draft(&shell, &cx, |d| {
        d.list_items("columns")
            .unwrap()
            .iter()
            .map(|i| i.name.clone())
            .collect()
    });
    assert_eq!(names, ["npv", "book"]);
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.row_label(d.selected_row().unwrap())),
        "book"
    );
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(400));
    cx.run_until_parked();
    let text =
        std::fs::read_to_string(dir.path().join("view_presentation.toml")).unwrap_or_default();
    assert!(text.contains("order = [\"npv\", \"book\"]"), "{text}");
}

/// An armed confirmation consumes row drops without editing. Use a user-owned object so
/// Delete arms a real confirmation.
#[gpui::test]
fn a_row_drop_does_nothing_while_a_confirm_is_armed(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_user_only_view(),
        dir.path(),
        "config::views",
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.confirm),
        Some(objectdialog::Confirm::Delete),
        "d arms delete on an object the user layer itself defines"
    );

    let (src, dst) = edit_draft(&shell, &cx, |d| {
        let rows = d.rows();
        let book = rows
            .iter()
            .copied()
            .find(|r| d.row_label(*r) == "book")
            .unwrap();
        let npv = rows
            .iter()
            .copied()
            .find(|r| d.row_label(*r) == "npv")
            .unwrap();
        (d.row_drag(book).unwrap(), d.row_drag(npv).unwrap())
    });
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            objectdialog::render::on_row_dropped(shell, &src, &dst, window, cx);
        });
    });
    cx.run_until_parked();

    assert_eq!(
        dialog_state(&shell, &cx, |s| s.confirm),
        Some(objectdialog::Confirm::Delete),
        "the armed delete must not be clobbered by a drop behind it"
    );
    let names: Vec<String> = edit_draft(&shell, &cx, |d| {
        d.list_items("columns")
            .unwrap()
            .iter()
            .map(|i| i.name.clone())
            .collect()
    });
    assert_eq!(
        names,
        ["book", "npv"],
        "the list must be untouched — the drop did nothing at all"
    );

    cx.executor()
        .advance_clock(std::time::Duration::from_millis(400));
    cx.run_until_parked();
    assert!(
        !dir.path().join("view_presentation.toml").exists(),
        "no write reaches disk from a drop claimed by an armed confirm"
    );
}

/// Clicking a completion replaces the trailing chain segment and appends ` / `,
/// matching Tab. The field keeps focus and chain mode remains active.
#[gpui::test]
fn clicking_a_completion_row_completes_the_chain(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_slot_3(&["book"]),
        dir.path(),
        "config::groupings",
    );
    cx.simulate_keystrokes("3 i");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.chain_entry()));
    // Open a fresh segment so `lhu` is offered.
    cx.simulate_input(" / ");
    cx.run_until_parked();
    let row = cx
        .debug_bounds("objectdialog-item-lhu")
        .expect("lhu is a completion");
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(40.0), row.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.query.clone()),
        "book / lhu / "
    );
    assert!(
        edit_draft(&shell, &cx, |d| d.chain_entry()),
        "the field is still open"
    );
    assert_eq!(
        dialog_input_text(&shell, &cx),
        "book / lhu / ",
        "the sync wrote the completion into the field"
    );
    assert!(cx.debug_bounds("dialog-mode-pill-chain").is_some());
}

/// Add a second available measure to the shared desk fixture. A catalogue-to-catalogue
/// drop needs distinct payloads; dropping the only available row onto itself must stay
/// silent. Replace the dataset by name so unrelated fixture document additions cannot
/// change which document is replaced.
fn services_with_two_available_columns() -> ShellServices {
    let mut services = test_services();
    let mut layered = desk_view_docs();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk_snapshot.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk_snapshot.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
         [risk_snapshot.columns.delta01]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
         [risk_snapshot.columns.gamma01]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
    )
    .unwrap();
    let slot = layered
        .iter()
        .position(|doc| doc.name == "datasets")
        .expect("the desk fixture has a datasets doc");
    layered[slot] = datasets;
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: layered,
        desk: None,
        user: None,
    });
    services
}

/// The two `Available` payloads of `tree`'s column list, plus the shell
/// its edit stage is open on.
fn open_tree_edit_stage_with_two_available(
    cx: &mut gpui::TestAppContext,
    dir: &std::path::Path,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_two_available_columns(),
        dir,
        "config::views",
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    (shell, cx)
}

/// The columns of `tree`'s list and its catalogue, as the draft stands.
fn columns_and_available(
    shell: &Entity<ShellView>,
    cx: &gpui::VisualTestContext,
) -> (Vec<String>, Vec<String>) {
    edit_draft(shell, cx, |d| {
        let items = d
            .list_items("columns")
            .unwrap()
            .iter()
            .map(|i| i.name.clone())
            .collect();
        let available = d
            .available_items("columns")
            .unwrap_or_default()
            .iter()
            .map(|i| i.name.clone())
            .collect();
        (items, available)
    })
}

/// Dropping an available row onto another available row explains that the catalogue is
/// unordered. Dropping onto itself stays silent because nothing moved; check that case
/// before the general catalogue-to-catalogue branch.
#[gpui::test]
fn a_catalogue_to_catalogue_drop_says_the_catalogue_has_no_order(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage_with_two_available(cx, dir.path());
    let (delta, gamma) = edit_draft(&shell, &cx, |d| {
        let rows = d.rows();
        let find = |name: &str| {
            rows.iter()
                .copied()
                .find(|r| {
                    matches!(r, objectdialog::EditRow::Available { .. }) && d.row_label(*r) == name
                })
                .unwrap_or_else(|| panic!("{name} should be an available row"))
        };
        (
            d.row_drag(find("delta01")).unwrap(),
            d.row_drag(find("gamma01")).unwrap(),
        )
    });

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            objectdialog::render::on_row_dropped(shell, &delta, &gamma, window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()),
        Some("the catalogue has no order".to_string())
    );
    assert_eq!(
        columns_and_available(&shell, &cx),
        (
            vec!["book".to_string(), "npv".to_string()],
            vec!["delta01".to_string(), "gamma01".to_string()]
        ),
        "and neither list moved"
    );

    // A self-drop is silent and clears any notice left by the preceding drop.
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            objectdialog::render::on_row_dropped(shell, &delta, &delta, window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()),
        None,
        "a row put back where it was says nothing at all"
    );

    cx.executor()
        .advance_clock(std::time::Duration::from_millis(400));
    cx.run_until_parked();
    assert!(
        !dir.path().join("view_presentation.toml").exists(),
        "and no inert drop writes"
    );
}

/// Resolve dragged names at drop time. If a name has disappeared, leave the list
/// unchanged and explain that the row is gone. A hand-built stale payload models
/// removal between grab and drop.
#[gpui::test]
fn a_drop_whose_name_has_left_the_list_says_that_row_is_gone(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    let dst = edit_draft(&shell, &cx, |d| {
        let book = d
            .rows()
            .iter()
            .copied()
            .find(|r| d.row_label(*r) == "book")
            .unwrap();
        d.row_drag(book).unwrap()
    });
    let gone = objectdialog::RowDrag {
        field: "columns".to_string(),
        own: true,
        name: "gone".to_string(),
    };

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            objectdialog::render::on_row_dropped(shell, &gone, &dst, window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()),
        Some("that row is gone".to_string())
    );
    let names: Vec<String> = edit_draft(&shell, &cx, |d| {
        d.list_items("columns")
            .unwrap()
            .iter()
            .map(|i| i.name.clone())
            .collect()
    });
    assert_eq!(names, ["book", "npv"], "and the list is untouched");

    cx.executor()
        .advance_clock(std::time::Duration::from_millis(400));
    cx.run_until_parked();
    assert!(
        !dir.path().join("view_presentation.toml").exists(),
        "a drop that resolved to nothing writes nothing"
    );
}

/// A `datasets` doc across two datasets, `risk` and `vol` — enough for
/// the browse list, and for `risk`'s `book` column to carry a layer.
fn services_with_schema() -> ShellServices {
    let mut services = test_services();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
         [vol.columns.underlying_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
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

/// Schema rows name the layer their value came from, and the names differ
/// in width (`builtin` against `user`). Each badge sits in a slot as wide
/// as the widest name, so a builtin column's value lines up with a
/// user-layer derived dimension's.
#[gpui::test]
fn schema_values_line_up_whatever_layer_each_row_names(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dimensions.toml"),
        "[desk]\nfrom = \"book\"\n[desk.values]\nBK000 = \"Flow\"\n",
    )
    .unwrap();
    let mut services = services_with_schema();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
    )
    .unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            datasets,
        ],
        desk: None,
        user: Some(dir.path().to_path_buf()),
    });
    let (_shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::schema");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("objectdialog-field-layer-columns.book")
            .is_some()
    );
    assert!(
        cx.debug_bounds("objectdialog-field-layer-derived.desk")
            .is_some()
    );
    let column = cx
        .debug_bounds("objectdialog-value-columns.book")
        .expect("book paints a value");
    let derived = cx
        .debug_bounds("objectdialog-value-derived.desk")
        .expect("the derived dimension paints a value");
    assert_eq!(
        column.right(),
        derived.right(),
        "a builtin row's value lines up with a user row's"
    );
    // The builtin row carries the widest name, so a slot sized for anything
    // narrower would spill its badge over its value.
    for (value, badge) in [
        (column, "objectdialog-field-layer-columns.book"),
        (derived, "objectdialog-field-layer-derived.desk"),
    ] {
        let badge_bounds = cx.debug_bounds(badge).unwrap();
        assert!(
            badge_bounds.left() >= value.right(),
            "{badge} sits in its own slot beside the value, not over it"
        );
    }
}

/// The Schema inspector lists datasets and provenance, then opens column rows.
/// Definition-changing actions remain read-only, while Enter opens the writable
/// column-presentation stage. The separate column-stage test also checks that returning
/// to Schema preserves its read-only gates.
#[gpui::test]
fn the_schema_inspector_lists_datasets_and_refuses_every_verb(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell_with(cx, services_with_schema(), "config::schema");
    assert!(cx.debug_bounds("objectdialog-row-risk").is_some());
    assert!(cx.debug_bounds("objectdialog-row-vol").is_some());

    cx.simulate_keystrokes("n");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some(objectdialog::READ_ONLY_NOTICE)
    );
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    ));

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-field-columns.book").is_some());
    assert!(
        cx.debug_bounds("objectdialog-field-layer-columns.book")
            .is_some(),
        "the row's own layer badge"
    );
    assert!(
        cx.debug_bounds("objectdialog-dest-columns.book").is_none(),
        "no doc badge on a read-only row"
    );

    for key in ["space", "i", "d", "shift-j"] {
        cx.simulate_keystrokes(key);
        cx.run_until_parked();
        assert_eq!(
            dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
            Some(objectdialog::READ_ONLY_NOTICE),
            "{key}"
        );
        assert!(
            shell.read_with(&cx, |s, _| s.pending_config_write.is_none()),
            "{key} queued a write"
        );
    }
    // `/` still filters.
    cx.simulate_keystrokes("/");
    cx.simulate_input("pos");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-field-columns.position_ref")
            .is_some()
    );
    assert!(cx.debug_bounds("objectdialog-field-columns.book").is_none());
}

/// Schema browse offers no create button, and its value-chip handler refuses edits.
/// Call the handler directly: Schema's display-only Text rows render no chip, so this
/// checks the writable guard without claiming to test an unrendered click target.
#[gpui::test]
fn the_schema_domain_offers_no_n_button_and_the_chip_door_refuses(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell_with(cx, services_with_schema(), "config::schema");
    assert!(
        cx.debug_bounds("objectdialog-action-n").is_none(),
        "read-only: no n button on browse"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-field-columns.book").is_some());
    assert_eq!(edit_draft(&shell, &cx, |d| d.selected), 0);
    let fields_before = edit_draft(&shell, &cx, |d| d.fields.clone());

    // The chip's door on row 1, forward: the cursor moves (the click
    // selects, as on every domain), the notice is the read-only one —
    // never `step_selected_row`'s "nothing changes with space", which
    // would mean the gate had let the step through — and no write is
    // queued.
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            objectdialog::render::on_value_chip_clicked(shell, 1, true, window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(edit_draft(&shell, &cx, |d| d.selected), 1);
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some(objectdialog::READ_ONLY_NOTICE)
    );
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.fields.clone()),
        fields_before,
        "nothing stepped"
    );
    assert!(shell.read_with(&cx, |s, _| s.pending_config_write.is_none()));
}

/// `config::schema` on `risk`, with a writable user directory so a
/// column-stage write lands on disk: the browse list, then `enter` into
/// the dataset's column rows.
fn open_risk_columns(
    cx: &mut gpui::TestAppContext,
    dir: &std::path::Path,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_schema(), dir, "config::schema");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    (shell, cx)
}

/// Opening a schema column shows its dataset/column breadcrumb. Editing width writes
/// only the dataset-presentation overlay; Delete and Revert are refused, and returning
/// to the schema rows retains their read-only behavior.
#[gpui::test]
fn the_schema_column_row_opens_the_column_stage_and_writes_the_dataset_overlay(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_risk_columns(cx, dir.path());
    cx.simulate_keystrokes("enter"); // book
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Column { ref object, ref column } if object == "risk" && column == "book"
    ));
    assert_eq!(
        shell.read_with(&cx, |s, _| objectdialog::render::crumb_text(s)),
        "risk › book"
    );
    assert!(cx.debug_bounds("objectdialog-field-width").is_some());
    // Delete and Revert are refused by the column-stage gate, with the same wording as
    // the Views column stage.
    for key in ["d", "r"] {
        cx.simulate_keystrokes(key);
        cx.run_until_parked();
        assert_eq!(
            dialog_state(&shell, &cx, |s| s.notice.clone()),
            Some(format!("{key} is not a verb in a column's stage"))
        );
    }

    cx.simulate_keystrokes("j i"); // width
    cx.run_until_parked();
    assert_eq!(dialog_input_text(&shell, &cx), "auto");
    cx.simulate_keystrokes("backspace backspace backspace backspace");
    cx.simulate_input("160");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-field-provenance-width")
            .is_some(),
        "the stepped field reads `dataset`"
    );

    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("dataset_presentation.toml")).unwrap();
    assert!(written.contains("[risk.columns.book]"), "{written}");
    assert!(written.contains("width = 160"), "{written}");
    assert!(!written.contains("hidden"), "{written}");
    // Write only the presentation overlay. Neither a complete datasets document nor a
    // user-layer schema definition may be produced by this column edit.
    for other in [
        "datasets.toml",
        "views.toml",
        "view_presentation.toml",
        "overrides.toml",
    ] {
        assert!(
            !dir.path().join(other).exists(),
            "{other} was written by a dataset-overlay edit"
        );
    }
    assert!(
        !written.contains("role") && !written.contains("utf8"),
        "and the overlay holds no schema keys: {written}"
    );

    cx.simulate_keystrokes("escape"); // back to the column rows
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit { .. }
    ));
    let row_text = edit_draft(&shell, &cx, |d| {
        d.fields
            .iter()
            .find(|f| f.key == "columns.book")
            .map(|f| match &f.kind {
                objectdialog::FieldKind::Text(t) => t.clone(),
                _ => String::new(),
            })
    });
    assert!(
        row_text.as_deref().is_some_and(|t| t.ends_with("160 px")),
        "{row_text:?}"
    );
    assert!(
        !edit_draft(&shell, &cx, objectdialog::Draft::is_dirty),
        "the re-derived rows describe what is already on the batch"
    );

    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some(objectdialog::READ_ONLY_NOTICE),
        "the schema rows' own verbs stay read-only"
    );
}

/// Two datasets and one desk view over the first — the fixture the
/// catalogue-seed test below needs, and the only one here with a
/// steppable `dataset` field.
fn services_with_two_datasets_and_a_view() -> ShellServices {
    let mut services = test_services();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
         [vol.columns.underlying_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
         [vol.columns.strike]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
    )
    .unwrap();
    let views = LayerDoc {
        layer: Layer::Desk,
        name: "views".to_string(),
        file: "<test:desk>".into(),
        table: "[tree]\ndataset = \"risk\"\ngrouping = [\"book\"]\n\
                [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n"
            .parse()
            .unwrap(),
    };
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            datasets,
            views,
        ],
        desk: None,
        user: None,
    });
    services
}

/// Picking a dataset through typeahead refreshes the available-column catalogue,
/// matching the stepping path. After selecting `vol`, the rows must come from that
/// dataset rather than the previous one.
#[gpui::test]
fn a_picked_dataset_rebuilds_the_available_catalogue(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_two_datasets_and_a_view(),
        dir.path(),
        "config::views",
    );
    cx.simulate_keystrokes("enter"); // tree's edit stage, cursor on `dataset`
    cx.run_until_parked();
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.available_items("columns").map(|rows| {
            rows.iter().map(|i| i.name.clone()).collect::<Vec<_>>()
        })),
        Some(vec!["npv".to_string()]),
        "sanity: risk's own available block"
    );

    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    cx.simulate_input("vol");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.choice("dataset").map(str::to_string)),
        Some("vol".to_string()),
        "sanity: the pick landed on the other dataset"
    );
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.available_items("columns").map(|rows| {
            rows.iter().map(|i| i.name.clone()).collect::<Vec<_>>()
        })),
        Some(vec!["strike".to_string()]),
        "the available catalogue was rebuilt from the picked dataset, \
         not left holding risk's own columns"
    );
}

/// Available columns inherit dataset presentation from configuration including the
/// pending batch. Edit a schema column and switch to Views within the debounce to prove
/// a newly promoted column uses the pending overlay, not stale persisted config.
#[gpui::test]
fn a_dataset_switch_inside_the_debounce_seeds_the_catalogue_from_the_pending_write(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_two_datasets_and_a_view(),
        dir.path(),
        "config::schema",
    );
    // browse: risk, vol — `j` onto vol, `enter` into its column rows
    // (file order: underlying_ref, strike), `j enter` onto strike, the
    // one of the two a Views catalogue can offer (`schema_role_kind`
    // answers `None` for a key column, so it is never available).
    cx.simulate_keystrokes("j enter j enter");
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Column { ref object, ref column }
            if object == "vol" && column == "strike"
    ));
    cx.simulate_keystrokes("j i"); // width
    cx.run_until_parked();
    cx.simulate_keystrokes("backspace backspace backspace backspace");
    cx.simulate_input("160");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    // Still INSIDE the debounce: no `flush_config_write`, so the write
    // lives on `shell.pending_config_write` and nowhere else.
    assert!(
        shell.read_with(&cx, |s, _| s.pending_config_write.is_some()),
        "the edit is pending, which is the whole point of this test"
    );
    assert!(!dir.path().join("dataset_presentation.toml").exists());

    cx.simulate_keystrokes("escape escape escape");
    cx.run_until_parked();
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("config::views".to_string()), None, window, cx);
        });
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("enter"); // tree's edit stage, cursor on `dataset`
    cx.run_until_parked();
    cx.simulate_keystrokes("space"); // risk -> vol
    cx.run_until_parked();
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.choice("dataset").map(str::to_string)),
        Some("vol".to_string()),
        "sanity: the step landed on the other dataset"
    );

    let width = edit_draft(&shell, &cx, |d| {
        d.fields
            .iter()
            .find(|f| f.key == "columns")
            .and_then(|f| match &f.kind {
                objectdialog::FieldKind::OrderedList { available, .. } => available.as_ref(),
                _ => None,
            })
            .and_then(|rows| rows.iter().find(|i| i.name == "strike"))
            .and_then(|i| i.presentation.width)
    });
    assert_eq!(
        width,
        Some(160.0),
        "the rebuilt catalogue carries the dataset level as the PENDING \
         batch holds it, not as the last flush left it"
    );
}

/// Clicking a schema column row opens its column stage.
#[gpui::test]
fn a_click_on_a_schema_column_row_opens_the_column_stage(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_risk_columns(cx, dir.path());
    let bounds = cx
        .debug_bounds("objectdialog-field-columns.book")
        .expect("row painted");
    cx.simulate_click(
        gpui::point(
            bounds.origin.x + gpui::px(8.0),
            bounds.origin.y + gpui::px(2.0),
        ),
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Column { ref column, .. } if column == "book"
    ));
}

/// Leaving a column stage restores the cursor to that column's row. Use the second row
/// so resetting to index zero cannot satisfy the assertion.
#[gpui::test]
fn leaving_a_schema_column_stage_puts_the_cursor_back_on_its_row(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_risk_columns(cx, dir.path());
    cx.simulate_keystrokes("j enter");
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Column { ref column, .. } if column == "position_ref"
    ));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(edit_draft(&shell, &cx, |d| d.selected), 1);
}

/// Mouse tick and drop handlers enforce Schema's read-only gate just like keyboard
/// actions. Call the drop handler directly to verify refusal independently of whether a
/// read-only row can initiate a drag.
#[gpui::test]
fn a_drop_on_the_schema_inspector_is_refused(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell_with(cx, services_with_schema(), "config::schema");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let row = objectdialog::RowDrag {
        field: "columns.book".to_string(),
        own: true,
        name: "book".to_string(),
    };

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            objectdialog::render::on_row_dropped(shell, &row, &row, window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some(objectdialog::READ_ONLY_NOTICE)
    );
    assert!(
        shell.read_with(&cx, |s, _| s.pending_config_write.is_none()),
        "a drop on a read-only row queued a write"
    );
}

/// Two sources over two datasets provide distinct dataset ordering and a source with
/// every optional key for text-edit tests.
fn services_with_sources() -> ShellServices {
    let mut services = test_services();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [vol.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
    )
    .unwrap();
    let sources = LayerDoc::builtin(
        "sources",
        "[vols]\ndataset = \"vol\"\npaths = [\"/v/*.csv\"]\n\
         [live]\ndataset = \"risk\"\npaths = [\"/a/*.csv\"]\npoll_interval = \"2s\"\n",
    )
    .unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            datasets,
            sources,
        ],
        desk: None,
        user: None,
    });
    services
}

/// Source rows sort and display by dataset. Text editing seeds the field, rejects a bad
/// duration without closing it, and applies a valid duration through a user copy and
/// flush using the reader's supported spelling.
#[gpui::test]
fn sources_rows_are_dataset_first_and_i_types_a_duration(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_sources(), dir.path(), "config::sources");
    let live = cx.debug_bounds("objectdialog-row-live").unwrap();
    let vols = cx.debug_bounds("objectdialog-row-vols").unwrap();
    assert!(
        live.origin.y < vols.origin.y,
        "risk · live sorts before vol · vols"
    );

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    // Cursor to `poll_interval` (row 5: dataset, paths, readiness, polls, priority, poll_interval).
    cx.simulate_keystrokes("j j j j j");
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.text_entry.is_some() && !d.chain_entry()));
    assert!(dialog_filter_is_focused(&shell, &mut cx));
    assert_eq!(
        dialog_input_text(&shell, &cx),
        "2s",
        "seeded with the value"
    );
    assert!(cx.debug_bounds("dialog-mode-pill-edit").is_some());
    assert!(
        cx.debug_bounds("objectdialog-actions").is_none(),
        "no verbs while a field is open"
    );

    cx.simulate_input(" minutes");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        edit_draft(&shell, &cx, |d| d.text_entry.is_some()),
        "refused: still open"
    );
    assert!(
        dialog_state(&shell, &cx, |s| s.notice.clone())
            .unwrap()
            .contains("45s")
    );
    assert_eq!(
        dialog_input_text(&shell, &cx),
        "2s minutes",
        "the text is kept"
    );

    cx.simulate_keystrokes("escape");
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    cx.simulate_keystrokes("backspace backspace");
    cx.simulate_input("30s");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.text_entry.is_none()));
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_none(),
        "a builtin source forks without asking"
    );
    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("sources.toml")).unwrap();
    assert!(written.contains("poll_interval = \"30s\""), "{written}");
    assert!(written.contains("paths = [\"/a/*.csv\"]"), "{written}");
    // Applying the changed source through reload marks the difference from the startup
    // source baseline as restart-required.
    assert!(
        shell
            .read_with(&cx, |s, _| s.restart_required.clone())
            .is_some_and(|m| m.contains("sources")),
        "a sources write raises the restart-required stripe"
    );
}

/// Creating a source seeds its dataset and an available name from the selected row.
/// Empty paths leave it idle with a warning, not an error.
#[gpui::test]
fn n_on_sources_seeds_the_dataset_and_creates_an_idle_source(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_sources(), dir.path(), "config::sources");
    cx.simulate_keystrokes("j"); // vol · vols
    cx.simulate_keystrokes("n");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.naming_dataset.clone()).as_deref(),
        Some("vol")
    );
    assert_eq!(
        dialog_input_text(&shell, &cx),
        "vol",
        "the dataset's name, since no source holds it"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.choice("dataset").map(str::to_string)).as_deref(),
        Some("vol")
    );
    assert!(edit_draft(&shell, &cx, |d| d
        .diagnostics
        .iter()
        .all(|x| x.severity != geode_core::config::Severity::Error)));
    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("sources.toml")).unwrap();
    assert!(
        written.contains("[vol]\ndataset = \"vol\"\npaths = []"),
        "{written}"
    );
}

// Reader diagnostics mapped to field rows.

/// A column diagnostic paints a glyph on its field row and prefixes its header message
/// with that row's label. An object-level diagnostic whose path maps to no field
/// remains only in the header.
#[gpui::test]
fn a_column_diagnostic_flags_its_row(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    let views = LayerDoc::builtin(
        "views",
        "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n[[tree.columns]]\nname = \"delta\"\nformat = { precision = 99 }\n\
         [[tree.joins]]\non = [\"book\"]\n",
    )
    .unwrap();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.npv]\ntype = \"f64\"\nrole = \"dimension\"\n[risk.columns.delta]\ntype = \"f64\"\nrole = \"dimension\"\n",
    )
    .unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            views,
            datasets,
        ],
        desk: None,
        user: None,
    });
    let (shell, mut cx) = dialog_test_shell_with(cx, services, "config::views");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-diag-objectdialog-item-delta")
            .is_some(),
        "the glyph on delta's row"
    );
    assert!(
        cx.debug_bounds("objectdialog-diag-objectdialog-item-npv")
            .is_none()
    );
    // Flagged and unflagged rows must align their labels. A diagnostic glyph added as
    // another `justify_between` child would change spacing, so compare their label
    // origins directly.
    let delta_row = cx
        .debug_bounds("objectdialog-item-delta")
        .expect("delta's row is painted");
    let delta_label = cx
        .debug_bounds("objectdialog-label-objectdialog-item-delta")
        .expect("delta's label is painted");
    let npv_row = cx
        .debug_bounds("objectdialog-item-npv")
        .expect("npv's row is painted");
    let npv_label = cx
        .debug_bounds("objectdialog-label-objectdialog-item-npv")
        .expect("npv's label is painted");
    assert_eq!(
        delta_label.origin.x, npv_label.origin.x,
        "a flagged row's label starts at the same x as an unflagged row's"
    );
    // The label must start near the row's own left edge — under the bug
    // (glyph as a third `justify_between` child) it floated toward the
    // 640px row's centre instead. `40.` is generous (row padding + the
    // 12px glyph + its gap is well under half the row's width) without
    // being so loose it would pass under the centring bug too.
    for (row, label, name) in [
        (delta_row, delta_label, "delta"),
        (npv_row, npv_label, "npv"),
    ] {
        let offset = label.origin.x - row.origin.x;
        assert!(
            offset < px(40.),
            "{name}'s label starts {offset:?} from its row's left edge, not near it"
        );
    }
    let diags = edit_draft(&shell, &cx, |d| d.diagnostics.clone());
    assert!(
        diags
            .iter()
            .any(|d| d.path.as_deref() == Some("views.tree.columns.1.format.precision")),
        "{diags:?}"
    );
    assert!(
        diags
            .iter()
            .any(|d| d.path.as_deref() == Some("views.tree.joins")),
        "the join diagnostic is present: {diags:?}"
    );
    // The header line's prefix is exactly what `render.rs`'s header block
    // computes: `row_for_path` resolved to a row, then `row_label`'d.
    // Checked through the same two calls rather than by reading painted
    // text — every header line shares the `objectdialog-diagnostic`
    // selector, so `debug_bounds` cannot tell one line's text from
    // another's.
    let (matched_prefix, unmatched_row, flagged_count) = edit_draft(&shell, &cx, |d| {
        let matched_prefix = d
            .diagnostics
            .iter()
            .find(|x| x.path.as_deref() == Some("views.tree.columns.1.format.precision"))
            .and_then(|x| x.path.as_deref())
            .and_then(|p| d.row_for_path("views", p))
            .map(|row| d.row_label(row));
        let unmatched_row = d
            .diagnostics
            .iter()
            .find(|x| x.path.as_deref() == Some("views.tree.joins"))
            .and_then(|x| x.path.as_deref())
            .and_then(|p| d.row_for_path("views", p));
        (matched_prefix, unmatched_row, d.flagged_rows("views").len())
    });
    assert_eq!(
        matched_prefix.as_deref(),
        Some("delta"),
        "the format diagnostic's header line is prefixed \"delta: \""
    );
    assert_eq!(
        unmatched_row, None,
        "the join diagnostic resolves to no row — its header line gets no prefix"
    );
    assert_eq!(
        flagged_count, 1,
        "only delta's row is flagged; the join diagnostic paints no glyph anywhere"
    );
}

/// Map a diagnostic's original column index to the column identity before applying
/// presentation order. With the two columns reversed for display, the glyph must still
/// mark `delta` rather than the row now occupying its original index.
#[gpui::test]
fn a_column_diagnostic_survives_a_reordered_presentation(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    let views = LayerDoc::builtin(
        "views",
        "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n[[tree.columns]]\nname = \"delta\"\nformat = { precision = 99 }\n",
    )
    .unwrap();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.npv]\ntype = \"f64\"\nrole = \"dimension\"\n[risk.columns.delta]\ntype = \"f64\"\nrole = \"dimension\"\n",
    )
    .unwrap();
    let presentation = LayerDoc::builtin(
        "view_presentation",
        "[tree]\norder = [\"delta\", \"npv\"]\n",
    )
    .unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            views,
            datasets,
            presentation,
        ],
        desk: None,
        user: None,
    });
    let (_shell, mut cx) = dialog_test_shell_with(cx, services, "config::views");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    // Presentation orders rows as `[delta, npv]`; resolving `columns.1` by the
    // displayed index would incorrectly mark `npv`.
    assert!(
        cx.debug_bounds("objectdialog-diag-objectdialog-item-delta")
            .is_some(),
        "the glyph stays on delta even though the presentation moved it to the front"
    );
    assert!(
        cx.debug_bounds("objectdialog-diag-objectdialog-item-npv")
            .is_none(),
        "npv must never be flagged for a diagnostic that names delta"
    );
}

/// If a flushed config merge is rejected, the file write still occurs and the status
/// explains both outcomes: the edit is on disk but not live.
#[gpui::test]
fn a_flush_the_merge_rejects_says_saved_but_rejected(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    // The invalid `ctrl` modifier alias rejects every reload over this user layer while
    // the edited object itself remains valid.
    std::fs::write(
        dir.path().join("app.toml"),
        "config_version = 1\n[keymap]\nmod = \"ctrl\"\n",
    )
    .unwrap();
    // `dialog_test_shell_in_dir` hands `user_dir` to `ShellView::new` as
    // the WRITE directory only; `services.config` is whatever was built,
    // so the user layer has to be loaded into it here.
    let mut services = test_services();
    let builtin = LayerDoc::builtin(
        "views",
        "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n[wide]\ndataset = \"risk\"\n[[wide.columns]]\nname = \"npv\"\n",
    )
    .unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            builtin,
        ],
        desk: None,
        user: Some(dir.path().to_path_buf()),
    });
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::views");
    cx.simulate_keystrokes("j enter"); // wide
    cx.run_until_parked();
    // Past the `Dataset` field row and the `Columns` field's own header
    // row (`open_tree_edit_stage`'s own comment names the same two rows
    // for the same reason), onto the `Columns` list's first — here only
    // — item: `npv`.
    cx.simulate_keystrokes("j j space"); // hide npv: a presentation write
    cx.run_until_parked();
    flush_config_write(&mut cx);
    let status = shell
        .read_with(&cx, |s, _| s.config_write_error.clone())
        .unwrap();
    assert!(
        status.starts_with(objectdialog::apply::REJECTED_STATUS),
        "{status}"
    );
    assert!(
        dir.path().join("view_presentation.toml").exists(),
        "the file was written regardless"
    );
}

/// The edit footer reflects the selected row: Sources Paths is editable text (`i`
/// only), while Dataset is a multi-option choice supporting both stepping and
/// typeahead.
#[gpui::test]
fn the_edit_footer_offers_i_only_where_a_row_can_take_it(cx: &mut gpui::TestAppContext) {
    let (_shell, mut cx) = dialog_test_shell_with(cx, services_with_sources(), "config::sources");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-hint-i").is_some(),
        "Sources opens on Dataset, a multi-option Choice i now opens"
    );
    assert!(
        cx.debug_bounds("objectdialog-hint-change").is_some(),
        "which the step keys also change"
    );
    cx.simulate_keystrokes("j"); // Paths, an editable Text
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-hint-i").is_some(),
        "Paths takes a typed value"
    );
    assert!(
        cx.debug_bounds("objectdialog-hint-change").is_none(),
        "and nothing on it steps"
    );
}

#[gpui::test]
fn the_edit_footer_hides_i_where_it_would_only_refuse(cx: &mut gpui::TestAppContext) {
    let (_shell, mut cx) = open_views_dialog(cx);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-hint-i").is_none(),
        "Views has no row i can open, so the footer must not teach it"
    );
}

// Column presentation editing.

/// Enter on a view member opens its column stage. Stepping writes a column overlay
/// without copying the view definition; Escape returns selection to the same column.
#[gpui::test]
fn the_column_stage_writes_a_differing_key_to_the_overlay(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    // `tree` is a DESK view of `book` and `npv`, with the cursor landing
    // on `book`; one `j` puts it on `npv`, whose name the overlay
    // assertions below read.
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    cx.simulate_keystrokes("j enter");
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Column { .. }
    ));
    assert_eq!(
        shell.read_with(&cx, |s, _| objectdialog::render::crumb_text(s)),
        "tree › npv"
    );
    assert!(cx.debug_bounds("objectdialog-field-scale").is_some());
    cx.simulate_keystrokes("j j space"); // label, width, scale → k
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_none(),
        "presentation never forks"
    );
    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("view_presentation.toml")).unwrap();
    assert!(
        written.contains("[tree.columns.npv]") && written.contains("scale = \"k\""),
        "{written}"
    );
    assert!(
        !dir.path().join("views.toml").exists(),
        "the desk's view is untouched"
    );

    // Clearing Label removes the override, reveals the inherited desk label, and
    // explains why. The overlay cannot delete a key supplied by the view definition,
    // and no empty label reaches the file.
    cx.simulate_keystrokes("k k"); // scale → width → label
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    assert_eq!(
        dialog_input_text(&shell, &cx),
        "NPV",
        "seeded with the label in force — the desk's"
    );
    cx.simulate_keystrokes("backspace backspace backspace enter");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| matches!(
        &d.fields[0].kind,
        objectdialog::FieldKind::Text(t) if t == "NPV"
    )));
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some("label follows the desk again")
    );
    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("view_presentation.toml")).unwrap();
    assert!(
        !written.contains("label"),
        "the overlay has no opinion about the label: {written}"
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit { .. }
    ));
    assert!(edit_draft(&shell, &cx, |d| matches!(
        d.selected_row(),
        Some(objectdialog::EditRow::Item { .. })
    )));

    // From a filtered column list, the first Enter keeps the query and
    // returns to normal mode; a second Enter opens the selected column.
    cx.simulate_keystrokes("/");
    cx.simulate_input("npv");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        matches!(
            dialog_state(&shell, &cx, |s| s.stage.clone()),
            objectdialog::Stage::Edit { .. }
        ),
        "the first enter only leaves the filter"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Column { .. }
    ));
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.mode),
        DialogMode::Normal,
        "the stage opens in normal mode whatever mode enter arrived in"
    );
}

/// A column-stage row names the layer its value comes from and no
/// destination: every field there writes the same overlay, so a per-row
/// `pres` says nothing. The layer badge sits in a slot of one width, so a
/// row that gains or lacks a badge keeps its value where every other
/// row's is.
#[gpui::test]
fn the_column_stage_badges_the_layer_in_one_aligned_slot(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = desk_view_services(&[(
        "dataset_presentation",
        "[risk_snapshot.columns.npv]\nscale = \"k\"\n",
    )]);
    let (_shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::views");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("j enter");
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("objectdialog-field-provenance-scale")
            .is_some()
    );
    assert!(
        cx.debug_bounds("objectdialog-field-provenance-width")
            .is_none()
    );
    for selector in [
        "objectdialog-dest-label",
        "objectdialog-dest-width",
        "objectdialog-dest-scale",
    ] {
        assert!(
            cx.debug_bounds(selector).is_none(),
            "{selector}: the column stage paints no destination badge"
        );
    }
    let scale = cx
        .debug_bounds("objectdialog-value-scale")
        .expect("scale paints a value");
    let width = cx
        .debug_bounds("objectdialog-value-width")
        .expect("width paints a value");
    assert_eq!(
        scale.right(),
        width.right(),
        "a badged row's value lines up with an unbadged row's"
    );
    let badge = cx
        .debug_bounds("objectdialog-field-provenance-scale")
        .expect("scale names its layer");
    assert!(
        badge.left() >= scale.right(),
        "the badge sits in its own slot beside the value, not over it"
    );
}

/// The Views column stage resolves dataset presentation beneath its view overlay.
/// Clearing a label override reveals the dataset value, reseeds the field from it, and
/// does not copy that inherited value into the view overlay.
///
/// Debug selectors establish whether a provenance chip exists, not its text. The pure
/// `dataset_columns::provenance_of` tests check which layer is named.
#[gpui::test]
fn clearing_a_view_label_says_it_follows_the_dataset(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = desk_view_services(&[(
        "dataset_presentation",
        "[risk_snapshot.columns.npv]\nlabel = \"NPV k\"\nscale = \"k\"\n",
    )]);
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::views");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    // Opening selects book; one j reaches npv, the column with the dataset overlay.
    cx.simulate_keystrokes("j enter");
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |s, _| objectdialog::render::crumb_text(s)),
        "tree › npv"
    );

    // Capture the desk and dataset layers separately below the view overlay. Their
    // original keys identify the inherited source for notices, while provenance
    // compares the current field against those lower layers.
    let layers = edit_draft(&shell, &cx, |d| {
        d.column_ctx.as_ref().map(|c| c.layers.clone())
    })
    .expect("the column stage carries its door's context");
    assert_eq!(layers.desk.label.as_deref(), Some("NPV"));
    assert_eq!(layers.dataset.label.as_deref(), Some("NPV k"));
    assert_eq!(
        layers.dataset.scale,
        Some(geode_core::view::Scale::Thousands)
    );

    assert!(
        cx.debug_bounds("objectdialog-field-provenance-scale")
            .is_some(),
        "scale is set at the dataset level, so the chip names a layer"
    );
    assert!(
        cx.debug_bounds("objectdialog-field-provenance-width")
            .is_none(),
        "width is set at no layer and untouched, so there is nothing to name"
    );

    cx.simulate_keystrokes("i"); // label
    cx.run_until_parked();
    assert_eq!(
        dialog_input_text(&shell, &cx),
        "NPV k",
        "seeded with the dataset's label, which beats the desk's `NPV`"
    );
    cx.simulate_keystrokes("backspace backspace backspace backspace backspace");
    cx.simulate_input("mine");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    // Flushed here so the clear below is asserted against a file that
    // HAS a `label` to lose: `!written.contains("label")` over a
    // `view_presentation.toml` that was never written passes for the
    // wrong reason, and would pass just as happily if the clear had
    // never worked.
    flush_config_write(&mut cx);
    let path = dir.path().join("view_presentation.toml");
    let written = std::fs::read_to_string(&path).expect("the diverging label reached disk");
    assert!(written.contains("label = \"mine\""), "{written}");

    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    cx.simulate_keystrokes("backspace backspace backspace backspace enter");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some("label follows the dataset again")
    );
    assert_eq!(dialog_input_text(&shell, &cx), "", "the field is closed");
    assert_eq!(
        edit_draft(&shell, &cx, |d| d
            .fields
            .iter()
            .find(|f| f.key == "label")
            .map(|f| match &f.kind {
                objectdialog::FieldKind::Text(t) => t.clone(),
                _ => String::new(),
            })),
        Some("NPV k".into()),
        "re-seeded from the dataset level"
    );
    flush_config_write(&mut cx);
    assert!(path.exists(), "the overlay file is still there to be read");
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(
        !written.contains("label"),
        "a cleared key is not written — {written}"
    );
}

/// A failed write rebuilds the object's draft from reverted config and exits the column
/// stage whose projected fields no longer exist. Make the on-disk file unparseable
/// while retaining valid in-memory config so only the write discovers the failure.
#[gpui::test]
fn a_failed_write_in_the_column_stage_steps_back_to_the_view(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    cx.simulate_keystrokes("j enter"); // npv's column stage
    cx.run_until_parked();
    std::fs::write(dir.path().join("view_presentation.toml"), "[tree\nhidden =").unwrap();
    cx.simulate_keystrokes("j j space"); // scale → k
    cx.run_until_parked();
    flush_config_write(&mut cx);

    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit { .. }
    ));
    assert!(edit_draft(&shell, &cx, |d| d.column().is_none()));
    let reported = shell.read_with(&cx, |s, _| s.config_write_error.clone());
    assert!(
        reported.as_deref().is_some_and(|m| m.contains("reverted")),
        "{reported:?}"
    );
}

/// Delete and Revert are unavailable inside a column stage because they act on the
/// whole object. Give the view a real presentation override so Revert would otherwise
/// arm. After Escape returns to the view, Revert must become available again.
#[gpui::test]
fn delete_and_revert_are_refused_in_the_column_stage(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    cx.simulate_keystrokes("j enter"); // npv's column stage
    cx.run_until_parked();
    cx.simulate_keystrokes("j j space"); // scale → k, a real override
    cx.run_until_parked();
    // The override has to reach MEMORY before either verb is asked
    // about it: an ordinary field edit applies at the debounced flush,
    // and `derive_rows` reads the live config — without this the row is
    // not `overridden` yet and `r` would have been refused anyway,
    // which would make the assertions below prove nothing.
    flush_config_write(&mut cx);

    for key in ["d", "r"] {
        cx.simulate_keystrokes(key);
        cx.run_until_parked();
        assert_eq!(
            dialog_state(&shell, &cx, |s| s.notice.clone()),
            Some(format!("{key} is not a verb in a column's stage")),
            "{key} answered about the view from inside a column's stage"
        );
        assert_eq!(
            dialog_state(&shell, &cx, |s| s.confirm),
            None,
            "{key} armed a confirm the crumb has navigated away from"
        );
        assert!(
            edit_draft(&shell, &cx, |d| d.column().is_some()),
            "{key} left the column stage"
        );
    }

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.column().is_none()));
    cx.simulate_keystrokes("r");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.confirm),
        Some(objectdialog::Confirm::Revert),
        "the refusal is the column stage's alone — r still arms on the view"
    );
}

/// Width is typed rather than stepped. `i` opens a field seeded with `auto`; an
/// in-range pixel count reaches the overlay, while invalid text keeps the field open
/// and names the allowed range.
#[gpui::test]
fn the_column_stages_width_is_typed_and_refused_out_of_range(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    cx.simulate_keystrokes("j enter"); // npv's column stage
    cx.run_until_parked();
    cx.simulate_keystrokes("j"); // label → width
    cx.run_until_parked();
    // List-only commands explain their unavailability in the current column stage.
    for (key, pressed) in [("x", "x"), ("shift-j", "shift+j"), ("shift-k", "shift+k")] {
        cx.simulate_keystrokes(key);
        cx.run_until_parked();
        assert_eq!(
            dialog_state(&shell, &cx, |s| s.notice.clone()),
            Some(format!("{pressed} is not a verb in a column's stage"))
        );
    }
    // Enter's notice points to this text row's edit command, `i`.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some("press i to type a value")
    );
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.text_entry.is_some()));
    assert_eq!(
        dialog_input_text(&shell, &cx),
        "auto",
        "seeded with the width in force"
    );

    // A word is not a width: refused, named, and the field stays open so
    // the typed text can be corrected rather than retyped.
    cx.simulate_keystrokes("backspace backspace backspace backspace");
    cx.simulate_input("wide");
    cx.run_until_parked();
    // Typing mirrors through the input change subscription without moving the cursor
    // off Width. Value editing must not reset selection as query filtering does.
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.selected),
        1,
        "typing into the open field must not move the cursor"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(notice.contains("20–2000"), "{notice}");
    assert!(edit_draft(&shell, &cx, |d| d.text_entry.is_some()));

    cx.simulate_keystrokes("backspace backspace backspace backspace");
    cx.simulate_input("160");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.text_entry.is_none()));
    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("view_presentation.toml")).unwrap();
    assert!(written.contains("width = 160"), "{written}");
}

// Colour configuration.

/// A builtin `colours` doc with one colour (`delta`, `hue = 240`) plus
/// the keymap — the same shape `services_with_sources` uses, so a first
/// edit to `delta` forks it exactly as a builtin source's first edit
/// does.
fn services_with_colours() -> ShellServices {
    let mut services = test_services();
    let colours = LayerDoc::builtin("colors", "[delta]\nhue = 240\n").unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            colours,
        ],
        desk: None,
        user: None,
    });
    services
}

/// Colour browse rows and edit headers paint theme-resolved swatches. Stepping hue
/// repaints the swatch, and creation refuses reserved names.
#[gpui::test]
fn the_colours_dialog_paints_swatches_and_refuses_reserved_names(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_colours(), dir.path(), "config::colors");
    assert!(cx.debug_bounds("objectdialog-swatch-delta").is_some());
    cx.simulate_keystrokes("n");
    cx.simulate_input("sign");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        dialog_state(&shell, &cx, |s| s.notice.clone())
            .unwrap()
            .contains("reserved")
    );
    cx.simulate_keystrokes("escape");
    // A `#` name is an absolute colour's spelling: refused the same way.
    cx.simulate_keystrokes("n");
    cx.simulate_input("#ff8800");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some("'#ff8800' is reserved")
    );
    cx.simulate_keystrokes("escape");
    cx.simulate_keystrokes("enter"); // delta
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-swatch-header").is_some());
    cx.simulate_keystrokes("space"); // hue 240 → 255
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| matches!(
        d.fields[0].kind,
        objectdialog::FieldKind::Number { value: 255, .. }
    )));
    // A builtin colour's first edit forks it without asking, exactly as
    // a builtin source's does.
    assert!(cx.debug_bounds("objectdialog-confirm").is_none());
    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("colors.toml")).unwrap();
    assert!(written.contains("[delta]\nhue = 255"), "{written}");
}

/// Ticking `tint_sign` turns the edit header's swatch into a triad —
/// the negative variant, the base, the positive variant — so the trader
/// sees the tint before any write lands; unticked, the header paints
/// the one swatch it always did and neither variant.
#[gpui::test]
fn ticking_tint_by_sign_paints_the_two_variant_swatches(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_colours(), dir.path(), "config::colors");
    cx.simulate_keystrokes("enter"); // delta
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-swatch-header").is_some());
    assert!(
        cx.debug_bounds("objectdialog-swatch-header-negative")
            .is_none()
    );
    assert!(
        cx.debug_bounds("objectdialog-swatch-header-positive")
            .is_none()
    );
    cx.simulate_keystrokes("j j j space"); // hue, tone, token → tint_sign, tick
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| matches!(
        d.fields[3].kind,
        objectdialog::FieldKind::Bool(true)
    )));
    let base = cx
        .debug_bounds("objectdialog-swatch-header")
        .expect("the base swatch stays");
    let negative = cx
        .debug_bounds("objectdialog-swatch-header-negative")
        .expect("the negative variant is painted");
    let positive = cx
        .debug_bounds("objectdialog-swatch-header-positive")
        .expect("the positive variant is painted");
    assert!(
        negative.origin.x < base.origin.x && base.origin.x < positive.origin.x,
        "in sign order: − base +"
    );
    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("colors.toml")).unwrap();
    assert!(
        written.contains("[delta]\nhue = 240\ntint_sign = true"),
        "{written}"
    );
}

/// A value-row double-click selects on the first press, then opens the same field as
/// `i` on the second. For a Choice row this is typeahead; a single click only selects.
#[gpui::test]
fn a_double_click_on_a_value_row_is_i(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_colours(), dir.path(), "config::colors");
    cx.simulate_keystrokes("enter"); // delta
    cx.run_until_parked();
    let token = cx
        .debug_bounds("objectdialog-field-token")
        .expect("the token row is painted");
    let at = gpui::point(
        token.origin.x + gpui::px(40.0),
        token.origin.y + gpui::px(4.0),
    );
    cx.simulate_mouse_down(at, MouseButton::Left, gpui::Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.selected_row()),
        Some(objectdialog::EditRow::Field(2)),
        "a single click selects the row"
    );
    assert!(
        !edit_draft(&shell, &cx, |d| d.text_entry.is_some()),
        "and opens nothing"
    );
    double_click(&mut cx, at, gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(
        edit_draft(&shell, &cx, |d| d.choice_entry()),
        "the double-click opened the typeahead, as `i` would"
    );
    assert!(
        dialog_filter_is_focused(&shell, &mut cx),
        "the field has the keys"
    );
    assert!(cx.debug_bounds("dialog-mode-pill-choose").is_some());
}

/// A double-click on a DOOR row (a Views member column) is "open the
/// column stage" and nothing more: the first mouse-down opens the stage,
/// and the second — landing one frame later on whatever field the new
/// stage painted at that point — must not open `i` on a row the trader
/// never aimed at (`ObjectDialogState::click_opened_stage`).
#[gpui::test]
fn a_double_click_on_a_door_row_opens_the_stage_and_nothing_more(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = open_views_dialog(cx);
    cx.simulate_keystrokes("enter"); // tree
    cx.run_until_parked();
    let member = cx
        .debug_bounds("objectdialog-item-delta01")
        .expect("the member row is painted");
    let at = gpui::point(
        member.origin.x + gpui::px(40.0),
        member.origin.y + gpui::px(4.0),
    );
    double_click(&mut cx, at, gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Column { .. }
    ));
    assert!(
        !edit_draft(&shell, &cx, |d| d.text_entry.is_some()),
        "the second click opened no field in the freshly opened stage"
    );
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
    // A fresh double-click INSIDE the stage is `i` again — the guard is
    // about the click that opened the stage, not the stage itself.
    let width = cx
        .debug_bounds("objectdialog-field-width")
        .expect("the column stage paints its width row");
    let at = gpui::point(
        width.origin.x + gpui::px(40.0),
        width.origin.y + gpui::px(4.0),
    );
    double_click(&mut cx, at, gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(
        edit_draft(&shell, &cx, |d| d.text_entry.is_some()),
        "a double-click on the width row opens its field"
    );
}

/// A browse-row double-click opens its edit stage once. Its second click must not
/// activate a control newly painted under the pointer, such as the grouping chain-field
/// row.
#[gpui::test]
fn a_double_click_on_a_browse_row_opens_the_edit_stage_and_nothing_more(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_slot_3(&["book"]),
        dir.path(),
        "config::groupings",
    );
    let row = cx
        .debug_bounds("objectdialog-row-3")
        .expect("slot 3's browse row is painted");
    let at = gpui::point(row.origin.x + gpui::px(40.0), row.origin.y + gpui::px(4.0));
    double_click(&mut cx, at, gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit { .. }
    ));
    assert!(
        !edit_draft(&shell, &cx, |d| d.text_entry.is_some()),
        "the second click opened no field in the freshly opened stage"
    );
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
}

/// Double-clicking a scope dimension opens Values once. Deliver distinct values between
/// clicks so the second click lands on a value row; it must not invoke that new row's
/// edit action or produce an unrelated notice.
#[gpui::test]
fn a_double_click_on_a_scopes_dimension_row_opens_its_values_and_not_a_field(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    cx.simulate_keystrokes("enter"); // mine
    cx.run_until_parked();
    let row = cx
        .debug_bounds("objectdialog-item-book")
        .expect("the book dimension row is painted");
    let at = gpui::point(row.origin.x + gpui::px(40.0), row.origin.y + gpui::px(4.0));
    // The pair's first click: an ordinary down, which opens the stage.
    cx.simulate_mouse_down(at, MouseButton::Left, gpui::Modifiers::none());
    cx.run_until_parked();
    let tag = dialog_state(&shell, &cx, |s| s.values_tag);
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Values { ref column, .. } if column == "book"
    ));
    shell.update(&mut cx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: SCOPES_KEY,
                tag,
                column: "book".into(),
                values: Ok(vec![("BK000".into(), 5), ("BK001".into(), 7)]),
            },
            cx,
        )
    });
    cx.run_until_parked();
    let under_pointer = cx
        .debug_bounds("objectdialog-item-BK000")
        .expect("the first value row is painted");
    assert!(
        under_pointer.contains(&at),
        "the pair's second click must land on a value row, got {under_pointer:?} for {at:?}"
    );
    // The pair's second click, the one the platform stamps `click_count: 2`.
    cx.update(|window, cx| {
        window.dispatch_event(
            gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                button: MouseButton::Left,
                position: at,
                modifiers: gpui::Modifiers::none(),
                click_count: 2,
                first_mouse: false,
            }),
            cx,
        );
        window.dispatch_event(
            gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                button: MouseButton::Left,
                position: at,
                modifiers: gpui::Modifiers::none(),
                click_count: 2,
            }),
            cx,
        );
    });
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Values { ref column, .. } if column == "book"
    ));
    assert!(
        edit_draft(&shell, &cx, |d| d.text_entry.is_none()),
        "the second click opened no field on the Values list"
    );
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.fields[0].key.clone()),
        "values",
        "the Values stage's own list is installed"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()),
        None,
        "and the second click was not answered as `i` on a value row"
    );
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
}

// Value chips, edit/create buttons, and confirmation guards.

/// The hue chip steps forward on click and backward on Shift-click through the normal
/// write path. Label clicks only select; an armed confirmation makes the chip inert.
#[gpui::test]
fn the_value_chip_steps_a_number_and_is_inert_under_a_confirm(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_colours(), dir.path(), "config::colors");
    cx.simulate_keystrokes("enter"); // delta
    cx.run_until_parked();
    let hue = |cx: &gpui::VisualTestContext| {
        edit_draft(&shell, cx, |d| match d.fields[0].kind {
            objectdialog::FieldKind::Number { value, .. } => value,
            _ => panic!("hue is a Number"),
        })
    };
    assert_eq!(hue(&cx), 240);

    let chip = cx
        .debug_bounds("objectdialog-value-hue")
        .expect("the hue chip paints");
    cx.simulate_click(chip.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(hue(&cx), 255, "click steps forward by the field's step");
    cx.simulate_click(
        chip.center(),
        gpui::Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    cx.run_until_parked();
    assert_eq!(hue(&cx), 240, "shift+click steps back");
    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("colors.toml")).unwrap();
    assert!(
        written.contains("[delta]\nhue = 240"),
        "the chip went through the write path: {written}"
    );

    // Label click: select only.
    let row = cx
        .debug_bounds("objectdialog-field-hue")
        .expect("row paints");
    cx.simulate_click(
        gpui::point(row.origin.x + gpui::px(20.0), row.center().y),
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();
    assert_eq!(hue(&cx), 240, "a row click does not step");
    let live_width = cx
        .debug_bounds("objectdialog-value-hue")
        .expect("the live chip paints")
        .size
        .width;

    // Armed: the chip has no handler — and no fill. The two forms of
    // `dialog::value_chip` differ in more than the listener: the live
    // one is padded as a chip, the inert one is the bare value text, so
    // the same text measures narrower once the question is armed. That
    // is the one observation that sees the RENDER gate rather than the
    // handler's own guard behind it (both drop the click, so a click
    // alone cannot tell them apart).
    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-confirm").is_some());
    let chip = cx
        .debug_bounds("objectdialog-value-hue")
        .expect("still painted, as text");
    assert!(
        chip.size.width < live_width,
        "the armed value is plain text, not a chip: {:?} vs {live_width:?}",
        chip.size.width
    );
    cx.simulate_click(chip.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(hue(&cx), 240, "inert while the question stands");
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_some(),
        "and the question is still there — the click was dropped, not answered"
    );
}

/// `i` on a Choice opens typeahead in place of the rows. Typing narrows options; Enter
/// commits the highlighted option through the stepping path and restores the row
/// cursor. Editing a builtin colour creates and announces a user copy.
#[gpui::test]
fn i_on_a_choice_row_opens_a_typeahead_and_enter_picks_the_lit_option(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_colours(), dir.path(), "config::colors");
    cx.simulate_keystrokes("enter"); // delta
    cx.run_until_parked();
    cx.simulate_keystrokes("j j"); // hue → tone → token
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.choice_entry()));
    assert!(
        dialog_filter_is_focused(&shell, &mut cx),
        "the field has the keys"
    );
    assert_eq!(dialog_input_text(&shell, &cx), "", "opens empty");
    assert!(cx.debug_bounds("dialog-name-row").is_some());
    assert!(cx.debug_bounds("dialog-mode-pill-choose").is_some());
    assert!(
        cx.debug_bounds("objectdialog-choice-list").is_some(),
        "options in the list's place"
    );
    assert!(
        cx.debug_bounds("objectdialog-field-hue").is_none(),
        "no field rows while choosing"
    );
    assert!(
        cx.debug_bounds("objectdialog-actions").is_none(),
        "the action bar is withdrawn"
    );

    cx.simulate_input("dan");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-choice-danger").is_some());
    assert!(
        cx.debug_bounds("objectdialog-choice-accent").is_none(),
        "narrowed away"
    );

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(!edit_draft(&shell, &cx, |d| d.choice_entry()));
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
    assert!(!dialog_filter_is_focused(&shell, &mut cx));
    assert_eq!(dialog_input_text(&shell, &cx), "");
    assert!(edit_draft(&shell, &cx, |d| matches!(
        &d.fields[2].kind,
        objectdialog::FieldKind::Choice { options, selected } if options[*selected] == "danger"
    )));
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.selected_row()),
        Some(objectdialog::EditRow::Field(2)),
        "the cursor is back on the token row"
    );
    assert!(
        cx.debug_bounds("objectdialog-field-hue").is_some(),
        "the field rows are back"
    );
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_none(),
        "a builtin forks without asking"
    );
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(notice.contains("copied 'delta'"), "{notice}");
    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("colors.toml")).unwrap();
    assert!(written.contains("token = \"danger\""), "{written}");
}

/// `tab` completes the lit option into the field, `up`/`down` move the
/// highlight, a row click is `tab`, and `escape` cancels with the value
/// untouched and the cursor on the row.
#[gpui::test]
fn tab_completes_and_escape_cancels_a_choice_field(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_colours(), dir.path(), "config::colors");
    // All Colours rows are stops in this fixture. Two j presses reach Token, whose i
    // command opens its choice typeahead.
    cx.simulate_keystrokes("enter j j i");
    cx.run_until_parked();
    // A click on a row OTHER than the currently lit one is `tab` on
    // THAT row — not a no-op replay of whatever the field already
    // holds. The highlight opens on "none" (the field's current
    // value); clicking "muted" (a different, unrelated row, still
    // painted since the query is empty) must narrow the field to
    // "muted" and stay open.
    let muted = cx.debug_bounds("objectdialog-choice-muted").unwrap();
    cx.simulate_mouse_down(
        muted.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();
    assert_eq!(dialog_input_text(&shell, &cx), "muted");
    assert!(
        edit_draft(&shell, &cx, |d| d.choice_entry()),
        "a click completes, it does not pick"
    );
    // Cancel and reopen fresh (query cleared, every option painted
    // again) for the down/tab dance below.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    let lit = edit_draft(&shell, &cx, |d| {
        d.choice
            .as_ref()
            .unwrap()
            .highlighted_text()
            .map(str::to_string)
    });
    assert_eq!(lit.as_deref(), Some("foreground"), "row 1 of the 16 tokens");
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert_eq!(dialog_input_text(&shell, &cx), "foreground");
    // A click on the (only) painted row is `tab` too.
    let bounds = cx.debug_bounds("objectdialog-choice-foreground").unwrap();
    cx.simulate_mouse_down(
        bounds.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();
    assert!(
        edit_draft(&shell, &cx, |d| d.choice_entry()),
        "a click completes, it does not pick"
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!edit_draft(&shell, &cx, |d| d.choice_entry()));
    assert!(edit_draft(&shell, &cx, |d| matches!(
        &d.fields[2].kind,
        objectdialog::FieldKind::Choice { options, selected } if options[*selected] == "none"
    )));
    assert!(
        shell.read_with(&cx, |s, _| s.pending_config_write.is_none()),
        "nothing queued by a cancel"
    );
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.selected_row()),
        Some(objectdialog::EditRow::Field(2))
    );
    assert!(
        cx.debug_bounds("objectdialog-hint-i").is_some(),
        "the footer teaches i on a Choice row"
    );
}

/// Schema's read-only rows refuse `i` without opening a choice field.
#[gpui::test]
fn i_is_refused_on_the_schema_inspector(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_two_datasets_and_a_view(),
        dir.path(),
        "config::schema",
    );
    cx.simulate_keystrokes("enter i");
    cx.run_until_parked();
    assert!(!edit_draft(&shell, &cx, |d| d.text_entry.is_some()));
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(notice.contains("open a column"), "{notice}");
}

/// Column-stage Choice fields use typeahead and advertise choosing a value in the
/// footer. Read `render::i_hint_word` directly for the wording because debug selectors
/// identify key chips, not hint text; separately assert the field actually opens as
/// typeahead.
#[gpui::test]
fn the_column_stages_choice_rows_teach_choose_and_i_opens_the_typeahead(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    // Onto `npv`'s column stage: label(0), width(1), scale(2),
    // precision(3), thousands(4), negative(5), color(6).
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Column { .. }
    ));
    cx.simulate_keystrokes("j j"); // label -> width -> scale
    cx.run_until_parked();
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.selected_row()),
        Some(objectdialog::EditRow::Field(2)),
        "on scale"
    );
    assert_eq!(
        edit_draft(&shell, &cx, |d| d
            .selected_vocabulary(objectdialog::Domain::Views)),
        objectdialog::RowVocabulary::StepsAndTypes,
        "a multi-option Choice steps and types"
    );
    assert!(cx.debug_bounds("objectdialog-hint-i").is_some());
    let word = edit_draft(&shell, &cx, |d| {
        objectdialog::render::i_hint_word(d.selected_row(), d).to_string()
    });
    assert_eq!(word, "choose a value", "scale is a Choice row");

    // `i` opens this Choice row as typeahead.
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    assert!(
        edit_draft(&shell, &cx, |d| d.choice_entry()),
        "i on scale opens the typeahead, same as any other Choice row"
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!edit_draft(&shell, &cx, |d| d.choice_entry()));

    cx.simulate_keystrokes("j"); // scale -> precision
    cx.run_until_parked();
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.selected_row()),
        Some(objectdialog::EditRow::Field(3)),
        "on precision"
    );
    let word = edit_draft(&shell, &cx, |d| {
        objectdialog::render::i_hint_word(d.selected_row(), d).to_string()
    });
    assert_eq!(word, "type a value", "precision is a Number row");
}

/// The two keyboard-only verbs gain buttons: `i` on the edit stage's bar
/// when the selected row is one `i` opens, `n` on the browse stage.
#[gpui::test]
fn i_and_n_have_buttons_that_do_what_their_keys_do(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_colours(), dir.path(), "config::colors");
    let n = cx
        .debug_bounds("objectdialog-action-n")
        .expect("browse offers n as a button");
    cx.simulate_click(n.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(
        dialog_state(&shell, &cx, |s| matches!(
            s.stage,
            objectdialog::Stage::Naming
        )),
        "the n button opens the naming row"
    );
    assert!(
        dialog_filter_is_focused(&shell, &mut cx),
        "and the field took focus through the sync"
    );
    assert!(
        cx.debug_bounds("objectdialog-action-n").is_none(),
        "the button is withdrawn while naming"
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    cx.simulate_keystrokes("enter"); // delta — cursor on `hue`, a Number, which `i` opens
    cx.run_until_parked();
    let i = cx
        .debug_bounds("objectdialog-action-i")
        .expect("the edit bar offers i on a Number row");
    cx.simulate_click(i.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(
        edit_draft(&shell, &cx, |d| d.text_entry.is_some()),
        "the i button opens the value field"
    );
    assert!(dialog_filter_is_focused(&shell, &mut cx));
    assert!(
        cx.debug_bounds("objectdialog-action-i").is_none(),
        "no verbs while the field is open"
    );
    cx.simulate_keystrokes("escape j"); // tone, a two-option Choice
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-action-i").is_some(),
        "a multi-option Choice both steps and types now (spec 2026-09-19 §3.2)"
    );
}

/// The i button follows the selected row's typing vocabulary. A Scopes dimension is a
/// cursor stop for ticking and opening Values, but offers no typed-entry command. This
/// distinguishes row-based availability from a domain-wide i button.
#[gpui::test]
fn the_i_button_is_withheld_on_a_list_item_row(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    // Open mine, the only saved scope, on its first dimension, book.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.selected_row()),
        Some(objectdialog::EditRow::Item { field: 0, item: 0 })
    );
    assert_eq!(
        edit_draft(&shell, &cx, |d| d
            .selected_vocabulary(objectdialog::Domain::Scopes)),
        objectdialog::RowVocabulary::Item,
        "a list entry ticks with space and opens its values with enter — never i"
    );
    assert!(
        cx.debug_bounds("objectdialog-action-i").is_none(),
        "no i button on a row i cannot open"
    );
}

/// An armed confirmation consumes edit-row clicks without moving the cursor or opening
/// another stage.
#[gpui::test]
fn an_edit_row_click_is_dropped_while_a_confirm_is_armed(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    // A user-owned view, so `d` arms rather than pointing at the desk.
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_user_only_view(),
        dir.path(),
        "config::views",
    );
    // Opening selects book; j selects npv. Clicking book would move selection and open
    // its column stage if no confirmation were armed.
    cx.simulate_keystrokes("enter j");
    cx.run_until_parked();
    let before = edit_draft(&shell, &cx, |d| d.selected);
    assert_eq!(before, 3);
    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-confirm").is_some());

    // Confirmation blocks a member click that would otherwise select and open it.
    let row = cx
        .debug_bounds("objectdialog-item-book")
        .expect("a row paints");
    cx.simulate_click(
        gpui::point(row.origin.x + gpui::px(20.0), row.center().y),
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.selected),
        before,
        "the cursor did not move"
    );
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_some(),
        "the question still stands"
    );

    // A door row: no column stage opens, so nothing disarms the question
    // behind the trader's back.
    let door = cx
        .debug_bounds("objectdialog-item-npv")
        .expect("a member row paints");
    cx.simulate_click(
        gpui::point(door.origin.x + gpui::px(20.0), door.center().y),
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "mine".to_string()
        },
        "the column stage did not open"
    );
    assert_eq!(edit_draft(&shell, &cx, |d| d.selected), before);
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_some(),
        "and the question was not silently disarmed"
    );

    // The confirmation also consumes clicks on the frozen filter row.
    let frozen = cx
        .debug_bounds("dialog-filter-frozen")
        .expect("the frozen row still paints while a confirm is armed");
    cx.simulate_mouse_down(
        gpui::point(
            frozen.origin.x + gpui::px(20.0),
            frozen.origin.y + gpui::px(4.0),
        ),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.mode),
        DialogMode::Normal,
        "the frozen-row click did not enter filter mode over an open question"
    );
    assert!(
        !dialog_filter_is_focused(&shell, &mut cx),
        "and the filter did not take focus"
    );
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_some(),
        "the question still stands"
    );
}

/// The Sources create button derives its dataset and name from the row selected at
/// click time, matching `n`.
#[gpui::test]
fn the_n_button_seeds_a_new_source_from_the_cursor_row(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_sources(), dir.path(), "config::sources");
    cx.simulate_keystrokes("j"); // vol · vols
    cx.run_until_parked();
    let n = cx
        .debug_bounds("objectdialog-action-n")
        .expect("browse offers n");
    cx.simulate_click(n.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(dialog_state(&shell, &cx, |s| matches!(
        s.stage,
        objectdialog::Stage::Naming
    )));
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.naming_dataset.clone()).as_deref(),
        Some("vol")
    );
    assert_eq!(dialog_input_text(&shell, &cx), "vol");
}

/// Groupings' edit button opens the slot-wide chain field from any chooser row,
/// matching `i`. Browse offers no create button for the fixed slot list.
#[gpui::test]
fn the_i_button_opens_the_chain_field_on_groupings_and_n_is_withheld(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_slot_3(&["book"]),
        dir.path(),
        "config::groupings",
    );
    assert!(
        cx.debug_bounds("objectdialog-action-n").is_none(),
        "the slots are fixed — no button for a verb that only ever refuses"
    );
    cx.simulate_keystrokes("3");
    cx.run_until_parked();
    assert!(!edit_draft(&shell, &cx, |d| d.chain_entry()));
    let i = cx
        .debug_bounds("objectdialog-action-i")
        .expect("the chooser offers i on its slot row");
    cx.simulate_click(i.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(
        edit_draft(&shell, &cx, |d| d.chain_entry()),
        "the button opened the chain field, as the key does"
    );
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Filter);
    assert!(dialog_filter_is_focused(&shell, &mut cx));
    assert_eq!(
        dialog_input_text(&shell, &cx),
        "book",
        "seeded with the chain"
    );
}

/// The footer offers Enter in browse and on rows that open a column stage. Filter mode
/// uses Enter to keep the query and return to normal mode; opening a match then
/// requires another Enter. Column fields that only answer with a notice have no open
/// hint.
#[gpui::test]
fn the_footers_name_enter_where_it_opens_something(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_a_desk_view(), dir.path(), "config::views");
    assert!(
        cx.debug_bounds("objectdialog-hint-enter").is_some(),
        "browse, normal mode: enter opens the edit stage"
    );
    cx.simulate_keystrokes("/");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-hint-enter").is_some(),
        "browse, filter mode: enter still opens the highlighted row"
    );
    cx.simulate_keystrokes("escape enter");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-hint-enter").is_some(),
        "the Views edit stage lands on a member row, where enter opens its column stage \
         — the display-only dataset row above it is no longer a cursor stop"
    );
    cx.simulate_keystrokes("j j");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-hint-enter").is_none(),
        "past the members onto an available candidate, which has no column stage to open"
    );
    cx.simulate_keystrokes("k k enter");
    cx.run_until_parked();
    assert!(
        matches!(
            dialog_state(&shell, &cx, |s| s.stage.clone()),
            objectdialog::Stage::Column { .. }
        ),
        "the column stage opened"
    );
    assert!(
        cx.debug_bounds("objectdialog-hint-enter").is_none(),
        "inside a column stage enter only gives a notice, so it is not named"
    );
}

#[gpui::test]
fn the_schema_edit_footer_names_enter_and_the_notice_teaches_the_door(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, mut cx) = dialog_test_shell_with(cx, services_with_schema(), "config::schema");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-hint-enter").is_some(),
        "the Schema edit stage: enter opens a column row's stage"
    );
    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(notice.contains("open a column"), "{notice}");
    assert!(notice.contains("enter"), "{notice}");
}

// Step keys and row-sensitive footer hints.

/// The `scale` `Choice`'s selected index in an open column stage — the
/// one number every stepping-key assertion below reads. Found by key
/// rather than by position so a seventh column key inserted ahead of it
/// does not silently move the assertion onto another row.
fn scale_index(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> usize {
    edit_draft(shell, cx, |draft| {
        let field = draft
            .fields
            .iter()
            .find(|f| f.key == "scale")
            .expect("the column stage has a scale row");
        match &field.kind {
            objectdialog::FieldKind::Choice { selected, .. } => *selected,
            other => panic!("scale should be a Choice, got {other:?}"),
        }
    })
}

/// In normal mode, `l` and `h` step the selected row forward and backward through the
/// same `Draft::step_selected` path as Space and Shift-Space.
#[gpui::test]
fn l_and_h_step_the_selected_row_in_the_column_stage(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    // `npv`'s column stage, then past `label` and `width` onto `scale`.
    cx.simulate_keystrokes("j enter j j");
    cx.run_until_parked();
    let before = scale_index(&shell, &cx);

    cx.simulate_keystrokes("l");
    cx.run_until_parked();
    let forward = scale_index(&shell, &cx);
    assert_ne!(forward, before, "l steps the Choice forward");

    cx.simulate_keystrokes("h");
    cx.run_until_parked();
    assert_eq!(
        scale_index(&shell, &cx),
        before,
        "h steps it back to where it started"
    );
    assert!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).is_none(),
        "neither key is announced as 'not a verb here'"
    );
}

/// `tab`/`shift+tab` step in BOTH modes — the settings dialog's own rule,
/// now this stage's. The filter keeps focus and the query is untouched:
/// a step is a value change, not a filter keystroke. `h`/`l` stay
/// letters in filter mode, which is what a trader typing `hidden` into
/// the query depends on.
#[gpui::test]
fn tab_steps_the_selected_row_in_both_modes(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    cx.simulate_keystrokes("j enter"); // npv's column stage, cursor on `label`
    cx.run_until_parked();

    // An inert row names the actual command key. Space is typing in filter mode, so a
    // step refusal there must not describe it as the pressed step key.
    cx.simulate_keystrokes("/ tab");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some("nothing on this row changes with tab"),
        "label is a Text: tab steps nothing here, and says so in tab's own name"
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    cx.simulate_keystrokes("j j"); // width → scale
    cx.run_until_parked();
    let start = scale_index(&shell, &cx);

    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    let forward = scale_index(&shell, &cx);
    assert_ne!(forward, start, "tab steps forward in normal mode");
    cx.simulate_keystrokes("shift-tab");
    cx.run_until_parked();
    assert_eq!(scale_index(&shell, &cx), start, "shift+tab steps back");

    // Into filter mode, where the shared `Input` holds the keyboard.
    cx.simulate_keystrokes("/");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Filter);
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert_eq!(
        scale_index(&shell, &cx),
        forward,
        "tab steps forward in filter mode too"
    );
    cx.simulate_keystrokes("shift-tab");
    cx.run_until_parked();
    assert_eq!(scale_index(&shell, &cx), start, "and shift+tab back");
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.mode),
        DialogMode::Filter,
        "stepping never leaves filter mode"
    );
    assert!(
        dialog_filter_is_focused(&shell, &mut cx),
        "nor moves focus off the field the trader is typing into"
    );
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.query.clone()),
        "",
        "and puts no character into the query"
    );

    // The other two aliases are ordinary letters here.
    cx.simulate_keystrokes("l h");
    cx.run_until_parked();
    assert_eq!(
        scale_index(&shell, &cx),
        start,
        "h and l type in filter mode rather than stepping"
    );
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.query.clone()),
        "lh",
        "they reached the field as text"
    );
}

/// Footer hints follow the selected row's capabilities. The column stage provides
/// editable Text (`i` only), multi-option Choice (stepping and typeahead), and Number
/// (stepping and typing) side by side.
#[gpui::test]
fn the_edit_footer_names_only_what_the_selected_row_offers(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    // The reorder hint follows the selected item row's capabilities.
    assert!(
        cx.debug_bounds("objectdialog-hint-reorder").is_some(),
        "a member row can be reordered"
    );
    // An available candidate is a stop because space adds it, but it has no member
    // position to reorder. The inert Dataset choice and Columns header are skipped.
    cx.simulate_keystrokes("j j");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-hint-reorder").is_none(),
        "an available candidate has no item to move, so shift+j/shift+k must not be named"
    );
    cx.simulate_keystrokes("k"); // back to the second member row

    cx.simulate_keystrokes("enter"); // its column stage, cursor on `label`
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-hint-reorder").is_none(),
        "and the column stage has no list at all"
    );
    assert!(
        cx.debug_bounds("objectdialog-hint-i").is_some(),
        "label is an editable Text: i types a value"
    );
    assert!(
        cx.debug_bounds("objectdialog-hint-change").is_none(),
        "and nothing on it steps, so the change keys must not be named"
    );

    cx.simulate_keystrokes("j j"); // width → scale
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-hint-change").is_some(),
        "scale is a Choice: the step keys change it"
    );
    assert!(
        cx.debug_bounds("objectdialog-hint-i").is_some(),
        "and i opens a typeahead over its options (a multi-option Choice)"
    );

    cx.simulate_keystrokes("j"); // precision
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-hint-change").is_some(),
        "a Number steps"
    );
    assert!(
        cx.debug_bounds("objectdialog-hint-i").is_some(),
        "and takes a typed value"
    );

    // Filter mode names the one stepping pair a focused `Input` leaves
    // free, and drops `i` — which types an `i` there rather than opening
    // anything.
    cx.simulate_keystrokes("/");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-hint-change").is_some(),
        "tab still steps the Number with the filter focused"
    );
    assert!(
        cx.debug_bounds("objectdialog-hint-i").is_none(),
        "i is a character in filter mode, so the footer must not name it"
    );
    // And on a row nothing steps, the filter footer names no step key
    // either: `escape` back to normal is the whole of it.
    cx.simulate_keystrokes("escape k k k"); // precision → scale → width → label
    cx.simulate_keystrokes("/");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-hint-change").is_none(),
        "label has nothing tab could step"
    );
}

/// Scope dimensions cannot be reordered, so their footer omits move-item keys despite
/// using the same item-row vocabulary as reorderable domains.
#[gpui::test]
fn the_scopes_dimensions_list_offers_no_reorder_chip(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    cx.simulate_keystrokes("enter");
    // The stage opens on book, skipping the inert Dimensions header.
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-hint-reorder").is_none(),
        "a scope's selections have no order, so the reorder chip must not paint"
    );
}

/// Clicking an available dimension's tick opens Values through the same path as Space.
/// It must not add an empty selection by falling through to the generic toggle
/// operation.
#[gpui::test]
fn clicking_an_available_dimensions_tick_opens_its_values_stage(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope_and_an_available_dimension(),
        dir.path(),
        "config::scopes",
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let tick = cx
        .debug_bounds("objectdialog-tick-lhu")
        .expect("lhu is available");
    cx.simulate_mouse_down(
        gpui::point(tick.origin.x + gpui::px(4.0), tick.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Values { ref column, .. } if column == "lhu"
    ));
}

// Delete and revert from browse.

/// [`services_with_sources`] plus one source the USER layer defines, so a
/// browse row exists whose `layer` is `Layer::User` and `d` has something
/// of the trader's own to delete — the case the request was made for.
fn services_with_a_user_source() -> ShellServices {
    let mut services = services_with_sources();
    let user = LayerDoc {
        layer: Layer::User,
        name: "sources".to_string(),
        file: "<test:user>".into(),
        table: "[mine]\ndataset = \"risk\"\npaths = [\"/m/*.csv\"]\n"
            .parse()
            .unwrap(),
    };
    let mut builtin = services.builtin.clone();
    builtin.push(user);
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin,
        desk: None,
        user: None,
    });
    services
}

/// `d` on the browse list's selected row arms the same delete confirm the
/// edit stage's `d` does, and confirming it removes the object without
/// the trader ever having opened it — the dialog stays in browse, the row
/// is gone, and the notice and file are the edit stage's own.
#[gpui::test]
fn d_in_the_browse_list_deletes_the_selected_user_source(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_user_source(),
        dir.path(),
        "config::sources",
    );
    // Rows sort `(dataset, name)`: `risk · live`, `risk · mine`, `vol · vols`.
    cx.simulate_keystrokes("j");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-row-mine").is_some());

    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_some(),
        "d arms the delete confirm from the browse list"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse,
        "without opening the object"
    );

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse,
        "and the dialog stays in browse after the removal"
    );
    assert!(
        cx.debug_bounds("objectdialog-row-mine").is_none(),
        "the row is gone from the list on the confirming keystroke"
    );
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_none(),
        "the question is answered"
    );
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        notice
            .as_deref()
            .is_some_and(|n| n.contains("deleted mine in sources.toml")),
        "the notice names the object and the file, got {notice:?}"
    );
    let written =
        std::fs::read_to_string(dir.path().join("sources.toml")).expect("the delete reaches disk");
    assert!(!written.contains("mine"), "{written}");
}

/// `r` on the browse list's selected row reverts a presentation-only
/// override exactly as the edit stage's `r` does, staying in browse.
#[gpui::test]
fn r_in_the_browse_list_reverts_the_selected_override(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = desk_view_services(&[("view_presentation", "[tree]\nhidden = [\"book\"]\n")]);
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::views");
    assert!(cx.debug_bounds("objectdialog-overridden-tree").is_some());

    cx.simulate_keystrokes("r");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_some(),
        "r arms the revert confirm from the browse list"
    );

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    );
    assert!(
        cx.debug_bounds("objectdialog-overridden-tree").is_none(),
        "the override badge is gone: the row is the desk's again"
    );
    assert!(
        cx.debug_bounds("objectdialog-row-tree").is_some(),
        "and the desk's view is still listed"
    );
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        notice
            .as_deref()
            .is_some_and(|n| n.contains("reverted tree in view_presentation.toml")),
        "got {notice:?}"
    );
    let written = std::fs::read_to_string(dir.path().join("view_presentation.toml")).unwrap();
    assert!(!written.contains("tree"), "{written}");
}

/// The browse list's `d`/`r` are gated by the selected row exactly as the
/// edit stage's are by the open object: on a desk row both refuse, with
/// the edit stage's own notices, and arm nothing.
#[gpui::test]
fn browse_d_and_r_refuse_on_a_desk_row_with_the_edit_stages_notices(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_a_desk_view(), dir.path(), "config::views");

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
        assert_eq!(
            dialog_state(&shell, &cx, |s| s.stage.clone()),
            objectdialog::Stage::Browse
        );
    }
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
}

/// A read-only domain refuses browse Delete and Revert with the same notice as its
/// other unavailable edit actions.
#[gpui::test]
fn browse_d_and_r_are_refused_on_a_read_only_domain(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell_with(cx, services_with_schema(), "config::schema");
    for key in ["d", "r"] {
        cx.simulate_keystrokes(key);
        cx.run_until_parked();
        let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
        assert_eq!(
            notice.as_deref(),
            Some(objectdialog::READ_ONLY_NOTICE),
            "{key} in browse on a read-only domain"
        );
        assert!(cx.debug_bounds("objectdialog-confirm").is_none());
    }
}

/// `escape` answers a browse confirm with "no": nothing is removed, the
/// dialog stays open in browse, and the row is untouched.
#[gpui::test]
fn escape_disarms_a_browse_confirm_and_deletes_nothing(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_user_only_view(),
        dir.path(),
        "config::views",
    );
    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-confirm").is_some());

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_none(),
        "escape disarms"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.object_dialog.is_some()),
        "and does not close the dialog — the question owned that escape"
    );
    assert!(cx.debug_bounds("objectdialog-row-mine").is_some());
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
}

/// While browse confirmation is armed, a row click cannot open another row or move
/// selection away from the confirmation target.
#[gpui::test]
fn a_row_click_is_dropped_while_a_browse_confirm_is_armed(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_user_source(),
        dir.path(),
        "config::sources",
    );
    cx.simulate_keystrokes("j d");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-confirm").is_some());

    click_selector(&mut cx, "objectdialog-row-vols");
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse,
        "the click must not open the row over an open question"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.selected),
        1,
        "nor move the cursor off the object the question is about"
    );
    assert!(cx.debug_bounds("objectdialog-confirm").is_some());
}

/// Browse Delete and Revert buttons appear only when the selected row supports them.
/// Clicking one arms the same confirmation as the corresponding key.
#[gpui::test]
fn the_browse_bar_offers_delete_and_revert_for_the_selected_row(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_user_source(),
        dir.path(),
        "config::sources",
    );
    // `live` (builtin) is selected: nothing of the trader's to delete.
    assert!(cx.debug_bounds("objectdialog-action-n").is_some());
    assert!(
        cx.debug_bounds("objectdialog-action-d").is_none(),
        "no delete button on a builtin row"
    );
    assert!(cx.debug_bounds("objectdialog-action-r").is_none());

    cx.simulate_keystrokes("j");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-action-d").is_some(),
        "the delete button follows the cursor onto the user-owned row"
    );
    assert!(
        cx.debug_bounds("objectdialog-action-r").is_none(),
        "a user-only object has no desk copy to revert to"
    );

    click_selector(&mut cx, "objectdialog-action-d");
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_some(),
        "the button is the mouse form of d"
    );
    assert!(
        cx.debug_bounds("objectdialog-action-n").is_none(),
        "and the confirm replaces the bar rather than joining it"
    );
}

/// The revert button appears on an overridden row.
#[gpui::test]
fn the_browse_bar_offers_revert_on_an_overridden_row(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = desk_view_services(&[("view_presentation", "[tree]\nhidden = [\"book\"]\n")]);
    let (_shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::views");
    assert!(cx.debug_bounds("objectdialog-action-r").is_some());
    assert!(
        cx.debug_bounds("objectdialog-action-d").is_none(),
        "the view itself is the desk's"
    );
    click_selector(&mut cx, "objectdialog-action-r");
    assert!(cx.debug_bounds("objectdialog-confirm").is_some());
}

/// A browse removal lands back in browse with the FILTER still applied
/// — a trader who typed `/ m i n e` to find the row is not done with the
/// filter because the row is gone — where a removal from the edit stage
/// walks `leave_edit`, which clears the query with the stage. The one
/// visible difference between the two landings, and the reason
/// `after_removal` branches rather than calling `leave_edit` outright.
#[gpui::test]
fn a_browse_removal_keeps_the_filter_applied(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_user_source(),
        dir.path(),
        "config::sources",
    );
    // Enter keeps the filter; Escape would restore the empty entry query.
    cx.simulate_keystrokes("/ m i n e enter");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.query.clone()), "mine");
    assert!(cx.debug_bounds("objectdialog-row-vols").is_none());

    cx.simulate_keystrokes("d enter");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-row-mine").is_none());
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.query.clone()),
        "mine",
        "the filter the trader typed survives the removal"
    );
    assert!(
        cx.debug_bounds("objectdialog-row-vols").is_none(),
        "so the list is still narrowed to it"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.mode),
        DialogMode::Normal,
        "in normal mode, where the verb was pressed"
    );
}

/// [`services_with_sources`] plus the given USER-layer sources, each
/// `(name, dataset)` — [`services_with_a_user_source`] generalised for
/// the tests below, which need a user-owned row in a particular sort
/// position.
fn services_with_user_sources(extra: &[(&str, &str)]) -> ShellServices {
    let mut services = services_with_sources();
    let text: String = extra
        .iter()
        .map(|(name, dataset)| {
            format!("[{name}]\ndataset = \"{dataset}\"\npaths = [\"/{name}/*.csv\"]\n")
        })
        .collect();
    let user = LayerDoc {
        layer: Layer::User,
        name: "sources".to_string(),
        file: "<test:user>".into(),
        table: text.parse().unwrap(),
    };
    let mut builtin = services.builtin.clone();
    builtin.push(user);
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin,
        desk: None,
        user: None,
    });
    services
}

/// After deleting the last browse row, clamp selection against configuration including
/// pending removal. The committed config is updated asynchronously, so reading it alone
/// would leave the cursor beyond the surviving rows.
#[gpui::test]
fn a_browse_delete_of_the_last_row_lands_the_cursor_on_the_new_last_row(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    // Sorted `(dataset, name)`: risk·live, vol·vols, vol·zz — `zz` last.
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_user_sources(&[("zz", "vol")]),
        dir.path(),
        "config::sources",
    );
    cx.simulate_keystrokes("j j");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.selected), 2);

    cx.simulate_keystrokes("d enter");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-row-zz").is_none());
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.selected),
        1,
        "the cursor lands on the new last row, not one past the end"
    );
    // And it is a real row: `enter` opens it.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "vols".to_string()
        }
    );
}

/// Confirmation records the target name, not only its browse index. If a reload changes
/// the row under the question, refuse the answer with a notice instead of deleting
/// another object.
#[gpui::test]
fn a_reload_under_an_armed_confirm_refuses_the_answer(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    // risk·live, risk·mine, vol·vols — `mine` at index 1.
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_user_sources(&[("mine", "risk")]),
        dir.path(),
        "config::sources",
    );
    cx.simulate_keystrokes("j d");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-confirm").is_some());

    // The reload: a second user source sorts into index 1 — risk·live,
    // risk·mina, risk·mine — so the index now names `mina`.
    let reloaded = services_with_user_sources(&[("mina", "risk"), ("mine", "risk")]);
    shell.update(&mut cx, |shell, cx| {
        shell.services.config = reloaded.config;
        shell.services.builtin = reloaded.builtin;
        cx.notify();
    });
    cx.run_until_parked();

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        notice.as_deref().is_some_and(|n| n.contains("changed")),
        "the answer is refused and says why, got {notice:?}"
    );
    assert!(cx.debug_bounds("objectdialog-confirm").is_none());
    assert!(
        cx.debug_bounds("objectdialog-row-mina").is_some()
            && cx.debug_bounds("objectdialog-row-mine").is_some(),
        "nothing was removed"
    );
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
}

/// `escape` while a browse confirm stands is the question's "no", never
/// the ladder's `ClearQuery` rung: with a query applied the query
/// survives the escape that disarms.
#[gpui::test]
fn escape_under_a_browse_confirm_disarms_and_keeps_the_query(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_user_source(),
        dir.path(),
        "config::sources",
    );
    cx.simulate_keystrokes("/ m i n e enter d");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-confirm").is_some());

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-confirm").is_none());
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.query.clone()),
        "mine",
        "the escape answered the question and did not clear the filter"
    );
    assert!(shell.read_with(&cx, |s, _| s.object_dialog.is_some()));
}

/// The bar's button is the one way a browse confirm can stand with the
/// live filter `Input` focused. The armed block claims the keys ahead
/// of the mode split, so a typed letter neither reaches the field nor
/// the object, and `enter` still answers.
#[gpui::test]
fn a_button_armed_confirm_in_filter_mode_owns_the_keys(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_user_source(),
        dir.path(),
        "config::sources",
    );
    cx.simulate_keystrokes("/ down");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Filter);
    assert_eq!(dialog_state(&shell, &cx, |s| s.selected), 1);

    click_selector(&mut cx, "objectdialog-action-d");
    assert!(cx.debug_bounds("objectdialog-confirm").is_some());

    cx.simulate_keystrokes("x");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.query.clone()),
        "",
        "a letter under the question is claimed, not typed"
    );
    assert!(cx.debug_bounds("objectdialog-confirm").is_some());

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-row-mine").is_none());
    assert!(
        !std::fs::read_to_string(dir.path().join("sources.toml"))
            .unwrap()
            .contains("mine")
    );
}

/// Groupings from browse: `d` on an empty slot names the browse remedy
/// (open it), not the edit stage's (tick a dimension) — there is nothing
/// to tick on the list.
#[gpui::test]
fn d_on_an_empty_slot_from_browse_names_the_browse_remedy(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) =
        dialog_test_shell_with(cx, services_with_slot_3(&["book"]), "config::groupings");
    // Slot 1 is empty; the cursor opens on it.
    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        notice
            .as_deref()
            .is_some_and(|n| n.contains("is empty") && n.contains("open it")),
        "got {notice:?}"
    );
    assert!(cx.debug_bounds("objectdialog-confirm").is_none());
}

/// Deleting from the edit stage selects the deleted row's surviving neighbor, matching
/// browse deletion instead of resetting to the first row.
#[gpui::test]
fn an_edit_stage_delete_lands_the_cursor_on_the_neighbour(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    // risk·live, risk·mine, vol·vols, vol·zz — delete `zz` (last).
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_user_sources(&[("mine", "risk"), ("zz", "vol")]),
        dir.path(),
        "config::sources",
    );
    cx.simulate_keystrokes("j j j enter");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "zz".to_string()
        }
    );
    cx.simulate_keystrokes("d enter");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    );
    assert!(cx.debug_bounds("objectdialog-row-zz").is_none());
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.selected),
        2,
        "the cursor lands on the new last row (vols), as a browse delete would"
    );
}

/// Confirmation renders its recorded object name even if a reload changes the row at
/// that index. The prompt and any stale-target refusal must refer to the same object.
#[gpui::test]
fn the_armed_prompt_names_the_recorded_target(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_user_sources(&[("mine", "risk")]),
        dir.path(),
        "config::sources",
    );
    cx.simulate_keystrokes("j d");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.confirm_target.clone()),
        Some("mine".to_string())
    );
    assert!(
        cx.debug_bounds("objectdialog-confirm-prompt-mine")
            .is_some()
    );

    let reloaded = services_with_user_sources(&[("mina", "risk"), ("mine", "risk")]);
    shell.update(&mut cx, |shell, cx| {
        shell.services.config = reloaded.config;
        shell.services.builtin = reloaded.builtin;
        cx.notify();
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-confirm-prompt-mine")
            .is_some(),
        "the prompt still names mine, the object the question is about"
    );
    assert!(
        cx.debug_bounds("objectdialog-confirm-prompt-mina")
            .is_none()
    );
}

// Context-sensitive field help.

/// The help line painted in the edit footer for the row under the cursor.
fn help_line(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> Option<String> {
    shell.read_with(cx, |shell, _| {
        let state = shell.object_dialog.as_ref()?;
        let draft = state.draft.as_ref()?;
        let key = draft.selected_field_key()?;
        let help = state.domain.help(&state.stage, key);
        (!help.is_empty()).then(|| help.to_string())
    })
}

/// The help line follows the cursor: two rows, two different sentences,
/// painted in the footer's own slot (`objectdialog-help`), never inside
/// the row list.
#[gpui::test]
fn the_help_line_follows_the_selected_row(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_sources(), dir.path(), "config::sources");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-help").is_some(),
        "the footer paints a help slot"
    );
    let dataset_help = help_line(&shell, &cx).expect("the dataset row has help");
    assert!(
        dataset_help.to_lowercase().contains("dataset"),
        "{dataset_help}"
    );

    cx.simulate_keystrokes("j j");
    cx.run_until_parked();
    let readiness_help = help_line(&shell, &cx).expect("the readiness row has help");
    assert_ne!(dataset_help, readiness_help, "each row explains itself");
    assert!(
        readiness_help.contains("sentinel"),
        "readiness names the sentinel convention, got {readiness_help}"
    );
    // The line sits in the footer, below the action bar, not in the list.
    let help = cx.debug_bounds("objectdialog-help").unwrap();
    let list = cx.debug_bounds("objectdialog-fields").unwrap();
    assert!(
        help.origin.y >= list.origin.y + list.size.height,
        "help paints below the row list"
    );
}

/// A notice takes the help line's slot for the keystroke it reports on,
/// and the help returns on the next one — one slot, so nothing shifts.
#[gpui::test]
fn a_notice_displaces_the_help_line_for_one_keystroke(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_sources(), dir.path(), "config::sources");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-help").is_some());
    // The hint chip BELOW the slot is what would move if the slot grew
    // or shrank — the slot's own `origin.y` is the footer's first child
    // either way and proves nothing.
    let chip_before = cx.debug_bounds("objectdialog-hint-change").unwrap();

    // `q` is no verb here: a notice.
    cx.simulate_keystrokes("q");
    cx.run_until_parked();
    assert!(dialog_state(&shell, &cx, |s| s.notice.is_some()));
    assert!(
        cx.debug_bounds("objectdialog-help").is_none(),
        "the notice owns the slot while it stands"
    );
    assert!(cx.debug_bounds("objectdialog-notice").is_some());
    let chip = cx.debug_bounds("objectdialog-hint-change").unwrap();
    assert_eq!(
        chip.origin.y, chip_before.origin.y,
        "one slot: the hints do not move"
    );

    // Two rows down is `readiness`, a `Choice` like `dataset`, so the
    // same change chip is there to measure against.
    cx.simulate_keystrokes("j j");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-help").is_some(),
        "and help returns"
    );
    let chip = cx.debug_bounds("objectdialog-hint-change").unwrap();
    assert_eq!(chip.origin.y, chip_before.origin.y);
}

/// A subscribed source alone in its doc, so the sweep below opens it
/// first and walks the four subscribed-only rows the directory-source
/// fixtures never paint (`document`, `topics`, `coalesce`, `source_time`).
/// `sources::fields` reads the raw table, so no adapter registry is
/// needed for the dialog to show them.
fn services_with_a_subscribed_source() -> ShellServices {
    let mut services = test_services();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[vol.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
    )
    .unwrap();
    let sources = LayerDoc::builtin(
        "sources",
        "[cvi]\ndataset = \"vol\"\nadapter = \"demo_bus\"\ndocument = \"cvi\"\n\
         topics = [\"marketdata/cvi/>\"]\n",
    )
    .unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            datasets,
            sources,
        ],
        desk: None,
        user: None,
    });
    services
}

/// A list item and an available row explain the list they belong to:
/// the columns list's own sentence, on every row of it.
#[gpui::test]
fn a_list_row_shows_its_lists_help(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    // The cursor is on the first column item: the COLUMNS field's own
    // sentence, byte for byte — not the dataset row's, which also
    // happens to mention columns (the first draft of this test matched
    // on the word and let a wrong-index mutant through).
    let columns_help = shell.read_with(&cx, |shell, _| {
        let state = shell.object_dialog.as_ref().unwrap();
        state.domain.help(&state.stage, "columns").to_string()
    });
    let item_help = help_line(&shell, &cx).expect("a column item has help");
    assert_eq!(item_help, columns_help);
    // Past the second item onto the available block's `delta01`.
    cx.simulate_keystrokes("j j");
    cx.run_until_parked();
    let row = edit_draft(&shell, &cx, |d| d.selected_row());
    assert!(
        matches!(row, Some(objectdialog::EditRow::Available { .. })),
        "the fixture's available block starts here, got {row:?}"
    );
    assert_eq!(help_line(&shell, &cx).as_deref(), Some(item_help.as_str()));
}

/// Every field on every domain — and every column-stage field — carries
/// a non-empty sentence, so a new field cannot ship silent. The sweep
/// opens each dialog's first object and walks its rows.
#[gpui::test]
fn every_field_on_every_domain_has_help(cx: &mut gpui::TestAppContext) {
    type Fixture = fn() -> ShellServices;
    // The third element names one row the fixture MUST have opened with,
    // so a case cannot pass vacuously — the subscribed source's four
    // extra rows in particular, which a directory source never paints.
    let cases: [(&str, Fixture, &str); 7] = [
        ("config::views", services_with_a_desk_view, "columns"),
        ("config::sources", services_with_sources, "batch_pattern"),
        (
            "config::sources",
            services_with_a_subscribed_source,
            "source_time",
        ),
        (
            "config::groupings",
            || services_with_slot_3(&["book"]),
            "dimensions",
        ),
        ("config::scopes", services_with_a_saved_scope, "dimensions"),
        ("config::colors", services_with_colours, "token"),
        ("config::schema", services_with_schema, "columns.book"),
    ];
    for (action, services, expected) in cases {
        let dir = tempfile::tempdir().unwrap();
        let (shell, mut cx) = dialog_test_shell_in_dir(cx, services(), dir.path(), action);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let missing: Vec<String> = shell.read_with(&cx, |shell, _| {
            let state = shell.object_dialog.as_ref().unwrap();
            let draft = state.draft.as_ref().unwrap();
            assert!(
                draft.fields.iter().any(|f| f.key == expected),
                "{action}: the fixture did not open with a '{expected}' row"
            );
            draft
                .fields
                .iter()
                .filter(|f| !help_fits(state.domain.help(&state.stage, &f.key)))
                .map(|f| f.key.clone())
                .collect()
        });
        assert!(
            missing.is_empty(),
            "{action}: fields without help, or over the width: {missing:?}"
        );
        cx.simulate_keystrokes("escape escape");
        cx.run_until_parked();
    }

    // The column stage (Views' door; Schema's opens the same seven).
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let missing: Vec<String> = shell.read_with(&cx, |shell, _| {
        let state = shell.object_dialog.as_ref().unwrap();
        assert!(matches!(state.stage, objectdialog::Stage::Column { .. }));
        let draft = state.draft.as_ref().unwrap();
        draft
            .fields
            .iter()
            .filter(|f| !help_fits(state.domain.help(&state.stage, &f.key)))
            .map(|f| f.key.clone())
            .collect()
    });
    assert!(
        missing.is_empty(),
        "column stage: fields without help, or over the width: {missing:?}"
    );
}

/// Non-empty and under the one-line slot's width: ~95 characters fit
/// `WIDTH` at the largest font size, so the tables keep to 90. A longer
/// sentence would clip (the slot never wraps), which is silent.
fn help_fits(help: &str) -> bool {
    !help.is_empty() && help.chars().count() <= 90
}

/// The filled slot is exactly one line of 1.25rem — `line_height` pinned
/// to the `min_h` — so an empty slot (the same `min_h`) is the same
/// height and the footer never moves between a row with help and one
/// without. gpui's default `phi()` line height would make a filled
/// slot ~2px taller than an empty one.
#[gpui::test]
fn the_help_slot_is_exactly_one_line(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_sources(), dir.path(), "config::sources");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let rem = cx.update(|window, _cx| window.rem_size());
    let help = cx.debug_bounds("objectdialog-help").unwrap();
    assert_eq!(help.size.height, rem * 1.25, "one line of 1.25rem");
}

/// Under an armed confirm the slot is blank: the confirm row is one
/// compact decision, and a sentence about whichever row the cursor is on
/// is noise beside it.
#[gpui::test]
fn the_help_line_is_blank_under_an_armed_confirm(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_user_only_view(),
        dir.path(),
        "config::views",
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-help").is_some());
    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-confirm").is_some());
    assert!(
        cx.debug_bounds("objectdialog-help").is_none(),
        "no help beside a question"
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(dialog_state(&shell, &cx, |s| s.confirm.is_none()));
    assert!(
        cx.debug_bounds("objectdialog-help").is_some(),
        "and it is back"
    );
}

/// Empty footer rows and an action bar with no buttons retain their height. Compare a
/// typed choice row with an inert text row, each isolated by a filter so both lists
/// have one row. With no stops remaining, the inert row can stay selected. The Go row
/// must remain within one pixel of its previous position; a missing footer row or
/// collapsed action bar would shift it substantially.
#[gpui::test]
fn the_footer_keeps_its_rows_when_the_selected_row_has_nothing_to_edit(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_sources(), dir.path(), "config::sources");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    // Filter to one row in each comparison so list height is comparable. Priority is a
    // Choice whose footer teaches value stepping.
    cx.simulate_keystrokes("/");
    cx.simulate_input("priority");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.visible_rows().len()),
        1,
        "sanity: the filter left only the priority row"
    );
    assert!(cx.debug_bounds("objectdialog-hint-change").is_some());
    let go_before = cx.debug_bounds("hint-row-go").unwrap();
    let edit_before = cx.debug_bounds("hint-row-edit").unwrap();

    // Filter to adapter alone. With no cursor stops left it remains selected, and its
    // empty edit-hint row must retain its height. Enter accepts the filter and returns
    // to Normal to expose that row's vocabulary.
    cx.simulate_keystrokes("/");
    cx.simulate_keystrokes("backspace backspace backspace backspace");
    cx.simulate_keystrokes("backspace backspace backspace backspace");
    cx.simulate_input("adapter");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.visible_rows().len()),
        1,
        "sanity: and now only the adapter row"
    );
    assert_eq!(
        edit_draft(&shell, &cx, |d| d
            .selected_vocabulary(objectdialog::Domain::Sources)),
        objectdialog::RowVocabulary::Inert
    );
    assert!(cx.debug_bounds("objectdialog-hint-change").is_none());
    let edit = cx.debug_bounds("hint-row-edit").unwrap();
    assert_eq!(
        edit.size.height, edit_before.size.height,
        "an empty row keeps a full row's height"
    );
    let go = cx.debug_bounds("hint-row-go").unwrap();
    // Allow subpixel layout differences between value-chip and plain-text rows. A
    // collapsed hint row or action bar would move the footer by much more.
    assert!(
        (go.origin.y - go_before.origin.y).abs() < gpui::px(1.0),
        "so nothing below it moves: {} vs {}",
        go.origin.y,
        go_before.origin.y
    );
}

// The title row's Back button: the pointer route for Escape's back rung.

/// Click the modal's Back button through a real pointer event.
fn click_back(cx: &mut gpui::VisualTestContext) {
    let back = cx
        .debug_bounds("shell-modal-back")
        .expect("the Back button paints");
    cx.simulate_click(back.center(), gpui::Modifiers::default());
    cx.run_until_parked();
}

/// Draw a fresh frame, then report whether the Back button painted in it.
fn back_paints(cx: &mut gpui::VisualTestContext) -> bool {
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.debug_bounds("shell-modal-back").is_some()
}

/// Browse is the first screen: nothing to go back to, so no Back button.
#[gpui::test]
fn the_back_button_is_absent_in_browse(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_a_desk_view(), dir.path(), "config::views");
    assert!(shell.read_with(&cx, |s, _| s.modal.is_some()));
    assert!(!back_paints(&mut cx), "browse has no parent screen");
}

/// A click leaves the edit stage for browse, as Escape's back rung does, with the
/// dialog still open. Typing afterwards proves the keyboard route is live: `/` enters
/// the browse filter and the typed text becomes its query.
#[gpui::test]
fn the_back_button_leaves_the_edit_stage_for_browse(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    assert!(back_paints(&mut cx), "the edit stage has a parent screen");

    click_back(&mut cx);
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    );
    assert!(shell.read_with(&cx, |s, _| s.modal.is_some()), "still open");
    assert!(!back_paints(&mut cx), "browse paints no Back button");
    assert!(cx.debug_bounds("objectdialog-row-tree").is_some());

    cx.simulate_keystrokes("/");
    cx.simulate_input("tr");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Filter);
    assert_eq!(dialog_state(&shell, &cx, |s| s.query.clone()), "tr");
}

/// From a column stage one click returns to the view's fields, not to browse.
#[gpui::test]
fn the_back_button_leaves_a_column_stage_for_its_view(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Column { .. }
    ));

    click_back(&mut cx);
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "tree".to_string()
        },
        "one screen back, not two"
    );
    assert!(back_paints(&mut cx), "the edit stage still has a parent");

    cx.simulate_keystrokes("/");
    cx.simulate_input("np");
    cx.run_until_parked();
    assert_eq!(edit_draft(&shell, &cx, |d| d.query.clone()), "np");
}

/// From the Values stage one click returns to the scope's fields.
#[gpui::test]
fn the_back_button_leaves_the_values_stage_for_the_scope(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    cx.simulate_keystrokes("enter"); // open `mine`
    cx.simulate_keystrokes("enter"); // Values stage on book
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Values { .. }
    ));

    click_back(&mut cx);
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "mine".to_string()
        }
    );

    cx.simulate_keystrokes("/");
    cx.simulate_input("bo");
    cx.run_until_parked();
    assert_eq!(edit_draft(&shell, &cx, |d| d.query.clone()), "bo");
}

/// From naming one click returns to browse with the half-typed name dropped, and the
/// field no longer owns the keys.
#[gpui::test]
fn the_back_button_cancels_naming(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_a_desk_view(), dir.path(), "config::views");
    cx.simulate_keystrokes("n");
    cx.simulate_input("half");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Naming
    );

    click_back(&mut cx);
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    );
    assert_eq!(dialog_input_text(&shell, &cx), "", "the name is dropped");
    assert!(!dialog_filter_is_focused(&shell, &mut cx));

    cx.simulate_keystrokes("/");
    cx.simulate_input("tr");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.query.clone()), "tr");
}

/// With a value field open and half typed, one click discards the typed text and
/// leaves the screen, both Escape rungs in one step. Normal mode afterwards proves the
/// field was cancelled rather than carried out of the stage: `/` is a command, not text.
#[gpui::test]
fn the_back_button_discards_an_open_field_and_leaves_in_one_click(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_sources(), dir.path(), "config::sources");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("j j j j j");
    cx.simulate_keystrokes("i");
    cx.simulate_input(" minutes");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.text_entry.is_some()));

    click_back(&mut cx);
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    );
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
    assert_eq!(dialog_input_text(&shell, &cx), "");
    assert!(!dialog_filter_is_focused(&shell, &mut cx));

    cx.simulate_keystrokes("/");
    cx.simulate_input("l");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.query.clone()), "l");
}

/// In a column stage with a field open, one click cancels the field and returns to the
/// view with no field open and no leftover text.
#[gpui::test]
fn the_back_button_cancels_a_column_field_before_leaving(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("i");
    cx.simulate_input("xyz");
    cx.run_until_parked();
    assert!(
        edit_draft(&shell, &cx, |d| d.text_entry.is_some()),
        "sanity: a field is open"
    );

    click_back(&mut cx);
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "tree".to_string()
        }
    );
    assert!(edit_draft(&shell, &cx, |d| d.text_entry.is_none()));
    assert_eq!(edit_draft(&shell, &cx, |d| d.query.clone()), "");
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
    assert!(!dialog_filter_is_focused(&shell, &mut cx));
}

/// With the edit filter active, one click leaves filter mode, drops the query, and
/// leaves the stage.
#[gpui::test]
fn the_back_button_leaves_an_edit_stage_while_filtering(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    cx.simulate_keystrokes("/");
    cx.simulate_input("np");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Filter);

    click_back(&mut cx);
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    );
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
    assert_eq!(dialog_input_text(&shell, &cx), "");

    cx.simulate_keystrokes("/");
    cx.simulate_input("t");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.query.clone()), "t");
}

/// A pending confirmation owns input: a Back click does nothing, and the question still
/// takes its answer from the keyboard afterwards.
#[gpui::test]
fn the_back_button_is_ignored_while_a_confirm_is_pending(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = desk_view_services(&[("view_presentation", "[tree]\nhidden = [\"book\"]\n")]);
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::views");
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("r");
    cx.run_until_parked();
    assert!(dialog_state(&shell, &cx, |s| s.confirm.is_some()));

    click_back(&mut cx);
    assert!(
        matches!(
            dialog_state(&shell, &cx, |s| s.stage.clone()),
            objectdialog::Stage::Edit { .. }
        ),
        "the click did not leave the stage"
    );
    assert!(
        dialog_state(&shell, &cx, |s| s.confirm.is_some()),
        "and the question is still asked"
    );

    cx.simulate_keystrokes("n");
    cx.run_until_parked();
    assert!(dialog_state(&shell, &cx, |s| s.confirm.is_none()));
    click_back(&mut cx);
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    );
}

/// Hovering the Back button names it and the key that takes the same step.
#[gpui::test]
fn hovering_the_back_button_names_escape(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    let back = cx
        .debug_bounds("shell-modal-back")
        .expect("the Back button paints");
    cx.simulate_mouse_move(
        back.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(600));
    cx.run_until_parked();
    assert!(cx.debug_bounds("tip-shell-modal-back-title").is_some());
    assert!(
        cx.debug_bounds("tip-shell-modal-back-chord-escape")
            .is_some()
    );
}
