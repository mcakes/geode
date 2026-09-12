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
    desk_view_services(&[])
}

/// That fixture plus `extra` user-layer documents, keyed by doc name.
///
/// Its one caller adds `view_presentation` and nothing else, which is
/// exactly the state §4.1's split produces and no `views.toml` fixture
/// can reach: a trader who hid a column has a user-layer file naming the
/// view while the view itself is still the desk's.
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

/// The desk-layer documents [`desk_view_services`] is built from: one
/// dataset and one desk view over it. Factored out so a fixture that
/// needs the SAME desk with a different `ConfigSources` — a user
/// directory holding a file that will not parse, say — does not have to
/// restate the desk and risk it drifting from every other test here.
///
/// `delta01` is on the dataset but not on `tree`'s own column list —
/// deliberately, so the view's edit stage has one column in its
/// available block (§18.2) without any fixture here having to build a
/// second dataset just to reach it.
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
        table: "[tree]\ndataset = \"risk_snapshot\"\ngrouping = [\"book\"]\n\
                [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n\
                [[tree.columns]]\nname = \"npv\"\n"
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
    // Past the `Dataset` row and onto the `Columns` list's first item.
    cx.simulate_keystrokes("j j");
    cx.run_until_parked();
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

/// §18.1: this dialog's mode pill lives in the modal's shared title row,
/// in BOTH stages. Browse used to paint a pill row of its own above the
/// filter and the edit stage painted none at all — `build` returns to
/// `build_edit` before ever reaching that row — so the two stages
/// disagreed about whether the dialog told you what mode it was in.
/// `title_extra` is set once in `open`, which is what makes them agree.
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
        // The available block (§18.2): on the dataset, not on the view.
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
        presentation.contains("hidden = [\"book\"]"),
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

    flush_config_write(&mut cx);

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
    // And there is nothing to announce: the change was applied on the
    // keystroke, so a notice would be reporting on something the screen
    // already shows.
    assert_eq!(dialog_state(&shell, &cx, |s| s.notice.clone()), None);
}

/// **The mirror image of the test above.** Hiding a member is
/// presentation and asks nothing; adding an AVAILABLE column changes what
/// the view IS, so it goes through the same fork confirm any other
/// definitional edit to a desk view does. `delta01` is on the dataset
/// (`desk_view_docs`) but not on `tree`'s own columns, so it is the one
/// row in the available block this fixture's `tree` has.
#[gpui::test]
fn adding_an_available_column_to_a_desk_view_asks_before_forking(cx: &mut gpui::TestAppContext) {
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
        cx.debug_bounds("objectdialog-confirm").is_some(),
        "membership forks a desk view"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    flush_config_write(&mut cx);

    let written = std::fs::read_to_string(dir.path().join("views.toml"))
        .expect("confirming forks the view into the user layer");
    assert!(written.contains("name = \"delta01\""), "{written}");
    let _ = shell;
}

/// **A draft whose own reader rejects it must not reach the batch.**
///
/// Spec §7.1's no-carry-forward rule means `reload::decide` rejects any
/// config holding an error diagnostic — so if an error-severity edit
/// joined the pending batch anyway, the flush's merge would be refused
/// while the file write still fired, leaving memory and disk disagreeing
/// (`objectdialog::apply`'s module doc, "the previous config's
/// diagnostics"). Nothing keyed today can make `Domain::validate` return
/// `Severity::Error` — `views::validate` only ever emits `Warning` (a
/// stale dataset name warns, by design, so a desk rename cannot break a
/// trader's personal file) — so this drives `Draft::diagnostics`
/// directly, exactly as the task brief allows: the rule still needs
/// pinning now, for Part 2b's `Text`, which will reach it through real
/// keys.
///
/// `shift+j` (`NormalCommand::MoveItem`) is the vehicle because it is the
/// one path into `commit_or_confirm` that does not call `revalidate`
/// first — `Toggle`/`ToggleBack` do, which would recompute the (all-
/// `Warning`) diagnostics and erase the injected error before the gate
/// ever saw it. Using it here does not claim `MoveItem` is where a real
/// error would be produced; it is only how this test reaches the gate
/// without recomputing over it.
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

/// **The gate is inside `commit_edit` itself, not only in front of it.**
///
/// `commit_or_confirm`'s own early check
/// (`apply::blocking_diagnostic`) is a UX nicety — it skips asking to
/// fork an edit that can never be saved — but `run_confirmed`'s
/// `Confirm::Fork` arm calls `apply::commit_edit` directly, bypassing
/// that early check entirely. This test answers the fork question
/// after the diagnostic turns to `Error`, which the real dialog cannot
/// do today (armed, every other key is claimed and dropped, so nothing
/// can call `revalidate` in between) — the point is to prove
/// `commit_edit` itself refuses regardless of *how* the draft came to
/// carry an error, rather than relying on that key-claiming behaviour
/// as the reason this call site is safe.
#[gpui::test]
fn a_confirmed_fork_still_refuses_an_error_diagnostic(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    // A second dataset, so the `Dataset` choice has somewhere to step to
    // and arms `Confirm::Fork` — same fixture as
    // `a_definitional_change_to_a_desk_view_confirms_before_forking`.
    let services = desk_view_services(&[(
        "datasets",
        "[other_snapshot.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
    )]);
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::views");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_some(),
        "forking a desk view has to ask first"
    );

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
            message: "dataset 'other_snapshot' does not exist".to_string(),
            path: None,
        }];
    });

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        shell.read_with(&cx, |shell, _| shell.pending_config_write.is_none()),
        "the second call site into commit_edit must refuse an error diagnostic exactly as the first one does"
    );
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        notice
            .as_deref()
            .is_some_and(|n| n.contains("other_snapshot")),
        "got {notice:?}"
    );

    flush_config_write(&mut cx);
    assert!(
        std::fs::read_dir(dir.path()).unwrap().next().is_none(),
        "no file may appear: the fork must not have been applied or written"
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

    // Past `Dataset` and onto the `Columns` list's first item, then hide
    // it — a real, ordinary presentation edit.
    cx.simulate_keystrokes("j j space");
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
    assert!(text.contains("hidden = [\"book\"]"), "{text}");
}

/// **The requirement, in one test.** Changing a config field is INSTANT:
/// the keystroke changes what the dialog shows, with no save key — and
/// the config and the file both follow on their own, together, a
/// debounce later.
///
/// "Instant" is the **dialog**, not every downstream consumer. Applying
/// the merged config per keystroke would emit `ShellEvent::ConfigReloaded`
/// per keystroke, and the app bridge turns that into new `ViewSpec`s —
/// so a held key would make every blotter tile requery at the OS
/// key-repeat rate, against a §7.1 budget of 50 ms at 1M rows. The
/// dialog's own response is free; the world catching up is not, so the
/// world catches up on the same timer the file does.
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
            .get("hidden")
            .and_then(|v| v.as_array())
            .map(|a| a.len()),
        Some(1),
        "hiding a column has to reach the merged config, got {applied:?}"
    );
    let text = std::fs::read_to_string(dir.path().join("view_presentation.toml"))
        .expect("and the file, on the same timer");
    assert!(text.contains("hidden = [\"book\"]"), "{text}");
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
/// batch. The success arm has to respect that sequence too. Clearing the
/// batch unconditionally loses any edit that arrived while the write was
/// in flight: the older write completes, erases the batch, and the newer
/// edit's own flush finds nothing to do — so it reaches neither memory
/// nor disk, and the watcher (woken by the write that *did* land) then
/// reverts memory to the older on-disk state. The trader's change
/// disappears with nothing on screen having said so.
///
/// **The race cannot be scheduled in a gpui test**, and pretending
/// otherwise would make this a test of the executor rather than of the
/// guard: the test executor polls a `background_executor().spawn` inline,
/// so `run_writes` and `finish_flush` run inside a single `tick()` with
/// no gap for a keystroke however finely the ticks are driven (measured —
/// an earlier version of this test ticked until the file appeared and
/// still found the success arm had already run). So the stale completion
/// is applied directly: a real `ShellView`, a real pending batch from a
/// real keystroke, the real `finish_flush`, and only the *scheduling*
/// synthesized. Then the batch is flushed for real and has to reach disk.
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
        objectdialog::apply::finish_flush(shell, seq.wrapping_sub(1), Ok(()), cx);
    });
    assert!(
        shell.read_with(&cx, |shell, _| shell.pending_config_write.is_some()),
        "a superseded flush's completion must not clear the batch a newer \
         edit is sitting in — that edit would reach neither memory nor disk"
    );

    flush_config_write(&mut cx);

    let text = std::fs::read_to_string(&file)
        .expect("the batch a stale completion left alone still has to be written");
    assert!(text.contains("hidden = [\"book\"]"), "{text}");
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
    cx.simulate_keystrokes("enter j j");
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

/// **The empty-table ruling.** A user's `view_presentation.toml` was found
/// holding a bare `[tree]` — a table that says nothing, which
/// `ViewPresentationSpec::apply` then warns about as a stale entry.
///
/// Under the staged model that took a save whose draft excluded nothing.
/// Under this one it is one keystroke: `views::presentation_table`
/// renders EMPTY whenever the trader's presentation matches the view's
/// own doc, so hiding a column and unhiding it produces exactly that
/// table — every time, instantly. So an empty rendering is written as an
/// **absence**: the object is removed from the user's document rather
/// than written as a table with nothing in it, in memory and on disk
/// alike — an *overlay* rendering only, which is the whole of
/// `apply::object_value`'s destination asymmetry: the same emptiness in a
/// domain's own doc writes nothing at all, because an absence there means
/// inherit rather than "nothing of mine to record".
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
        text.contains("hidden = [\"npv\"]"),
        "and it has to be the FINAL state, not the first edit of the run:\n{text}"
    );
    // Memory and the file agree, which is the only thing a coalesced
    // write is allowed to change about the result.
    let applied = presentation_of(&shell, &cx, "tree").expect("still personalised");
    assert_eq!(
        applied
            .get("hidden")
            .and_then(|v| v.as_array())
            .map(Vec::len),
        Some(1)
    );
}

/// A **definitional** change to an object the user's layer does not own
/// forks it into the user layer, and a fork freezes: the desk's next
/// column never reaches this trader (spec §4.1). It is the one edit that
/// still asks before acting — and declining takes the value back off the
/// screen, because a painted value that is neither applied nor persisted
/// is precisely what instant editing must never produce.
#[gpui::test]
fn a_definitional_change_to_a_desk_view_confirms_before_forking(cx: &mut gpui::TestAppContext) {
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
        cx.debug_bounds("objectdialog-confirm").is_some(),
        "forking a desk view has to ask first"
    );
    assert!(
        std::fs::read_dir(dir.path()).unwrap().next().is_none(),
        "and nothing may be applied or written while it asks"
    );

    cx.simulate_keystrokes("n");
    cx.run_until_parked();
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.choice("dataset").map(str::to_string)),
        Some("risk_snapshot".to_string()),
        "declining puts the field back — the screen may not keep a value \
         that was neither applied nor persisted"
    );

    cx.simulate_keystrokes("space enter");
    cx.run_until_parked();
    flush_config_write(&mut cx);
    let text = std::fs::read_to_string(dir.path().join("views.toml"))
        .expect("confirming forks the view into the user layer");
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

/// An unbound letter in the edit stage explains itself, like every other
/// key that deliberately does nothing here (`/`, `enter`, `i`, and a
/// `space` on a row with no value). A letter that is claimed, does
/// nothing and says nothing is the precise inert keystroke the
/// interaction model exists to eliminate — and it is worse in this stage
/// than in browse, because `d`/`r` have taught the user that letters act
/// here.
///
/// `z`, not `x`: §18.2 gave Views its own `x` (removing a member column),
/// so `x` on this fixture's first item (`book`, a member) now does
/// something instead of nothing. `z` is still unbound anywhere in this
/// stage.
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

/// **The undo for the commonest edit there is.** A desk view whose only
/// user-layer trace is a `view_presentation.toml` entry — a trader who
/// hid a column and nothing else — is overridden, and `r` reverts it.
///
/// Spec §5.3 assumed presentation always accompanies a doc override, so
/// both verbs were gated on markers derived from the `views` doc alone:
/// `r` answered "tree has no user override to revert" while the file it
/// would have removed sat on disk. `d` still refuses — the view itself is
/// the desk's — but it now names the verb that does work instead of
/// denying the user has anything.
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

/// **The requirement this task exists for.** `spawn_removals` wrote the
/// file and returned with no in-memory merge, so `d` on a user-layer
/// object left its row painting in the browse list until the 500 ms
/// watcher noticed the write and reloaded — every other mutation in this
/// dialog is instant, and a delete was the one exception.
///
/// The row must be gone **before the watcher would ever fire**: this
/// test never advances the clock at all (contrast `flush_config_write`,
/// which every file-asserting EDIT test above calls), so the only way it
/// can pass is if confirming the delete applies to memory — and reaches
/// disk — inside the same `run_until_parked()` that dispatches the `y`.
/// A version that routes the removal onto the 250 ms edit debounce
/// instead of an immediate flush would need a clock advance here and
/// fail exactly this assertion, which is the point: a delete is a single
/// already-confirmed act, not a keystroke stream to coalesce, so it has
/// nothing to wait for.
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

/// **Hazard from the task brief:** a removal now applies to memory before
/// its write completes, exactly like an edit — so a removal whose write
/// fails needs the same revert an edit's failed write already gets
/// ([`apply::revert_failed_write`]), or the object is gone from memory
/// and still sitting on disk with nothing having told the trader.
///
/// The fixture mirrors `a_failed_write_reverts_the_in_memory_change_and_
/// says_so`: the file on disk is unparseable, but the config already in
/// memory never read it back, so the in-memory removal succeeds and only
/// the write can discover the problem.
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

// --- Task 4: `config::groupings` ------------------------------------

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
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
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

/// §18.4: an unconfigured slot is a row, opening it shows every pickable
/// dimension unticked, and ticking the first writes the slot to the user
/// layer with NO fork question — there is no desk copy to fork.
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

    // Row 1 is selected on open. Open it, skip the read-only `Slot`
    // field AND the `Dimensions` header row (`rows()` emits one for
    // every field, `OrderedList` included, whether its items are empty
    // or not — the same two-`j` shape
    // `reordering_slot_3_and_pressing_ctrl_3_regroups_off_the_new_order`
    // needs to reach a configured slot's first item), then tick the
    // first dimension.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("j j space");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_none(),
        "nothing to fork"
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

/// **The point of Task 4.** `config::groupings` lists all nine slots
/// (§18.4), row 1 selected on open, so this test navigates down to slot
/// 3 before opening it; reordering its `dimensions` applies through the
/// whole pipeline — draft, pending batch, debounced flush,
/// `apply_reload`, `hot_reload::rebuild_slots` — and a later `ctrl+3`
/// regroups off the NEW order, never the one the slot opened with.
///
/// `Frame::active_grouping` is exactly what a following blotter tile
/// reads to regroup itself on `ctrl+1..9` (spec §4.2,
/// `geode_blotter::tile`'s own `last_grouping`), so asserting against it
/// — rather than only against `groupings.toml`'s bytes — is what proves
/// the edit reached the frame a following tile actually reads, not
/// merely the file underneath it. A weaker test asserting on the file
/// alone would still pass if some future refactor broke the flush's
/// `apply_reload` call without touching `run_writes`.
#[gpui::test]
fn reordering_slot_3_and_pressing_ctrl_3_regroups_off_the_new_order(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = services_with_slot_3(&["book", "lhu"]);
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::groupings");

    // Row 1 is selected on open (§18.4 — all nine slots list); navigate
    // down to slot 3, then into its edit stage.
    cx.simulate_keystrokes("j j");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    // Past `Slot` and the `Dimensions` header row, onto `book` — the
    // chain's first item — and swap it past `lhu`.
    cx.simulate_keystrokes("j j");
    cx.run_until_parked();
    cx.simulate_keystrokes("shift-j");
    cx.run_until_parked();

    // Every Groupings field is `Destination::Doc` (spec §8.2 — there is
    // no presentation split the way Views has one), so reordering a
    // builtin-owned slot is a definitional change to an object the user
    // layer does not own: it forks, and asks first, exactly like any
    // other `Doc` edit to a desk/builtin object.
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_some(),
        "reordering a builtin slot has to ask before forking it into the \
         user layer"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

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

    // Row 1 is selected on open (§18.4 — all nine slots list); navigate
    // down to slot 3 first.
    cx.simulate_keystrokes("j j");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("j j");
    cx.run_until_parked();
    cx.simulate_keystrokes("shift-j");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
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

/// **The whole-branch review's Major.** Unticking a slot's last dimension
/// asks for "this slot groups by nothing", and the config model has no
/// such state: `GroupingSlots::set` refuses an empty chain and
/// `GroupingSlots::from_doc` warns "slot N is empty; ignored". Worse, the
/// write it used to produce was a *removal* of the user-layer key, and in
/// a layered doc that means **inherit the layer beneath** — so the slot
/// silently went back to the desk's chain while the edit stage kept
/// painting an empty one and `ctrl+3` kept regrouping by the very chain
/// the trader had just cleared.
///
/// So the keystroke is declined (`Draft::step_selected`), and the
/// assertion is the *agreement* rather than the refusal: what the edit
/// stage paints is what a following tile groups by. A test that only
/// checked the notice, or only checked the file, could not see the
/// divergence — `reordering_slot_3_and_pressing_ctrl_3_regroups_off_the_new_order`
/// is the shape that can, and this is its negative twin.
#[gpui::test]
fn unticking_a_slots_last_dimension_leaves_the_painted_chain_and_the_frame_agreeing(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let services = services_with_slot_3(&["book"]);
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::groupings");

    // Row 1 is selected on open (§18.4 — all nine slots list); navigate
    // down to slot 3, into its edit stage, past `Slot` and the
    // `Dimensions` header, onto `book` — the chain's only ticked item.
    cx.simulate_keystrokes("j j");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("j j");
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

    // What the edit stage paints, which is the half that used to lie.
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

// ---------------------------------------------------------------------
// `Domain::Scopes` (Part 2a Task 5): the thinnest adapter, and its one
// new verb, `o`.
// ---------------------------------------------------------------------

/// A `scopes` doc with one saved scope, `mine`, selecting `book = BK001`
/// — deliberately different from whatever a test then puts on the
/// frame, so an assertion that the doc changed cannot pass by accident.
fn services_with_a_saved_scope() -> ShellServices {
    let mut services = test_services();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
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

/// **The point of Task 5.** `o` overwrites the saved scope under the
/// cursor with whatever the frame currently holds: the same
/// `commit_edit` → debounced flush → `apply_reload` → write pipeline
/// every other field edit goes through (spec §7.1), not a direct
/// `config_write` call — and the frame's own scope is untouched by it,
/// because `o` only ever writes config, never frame state (this is the
/// asymmetry `arm_overwrite`'s doc comment describes: the frame is the
/// input, the doc is the only thing written).
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
        cx.debug_bounds("objectdialog-confirm").is_some(),
        "o must ask before overwriting"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
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

/// `o` must confirm before acting, since it destroys the saved scope's
/// previous contents: pressing it alone must not touch the doc, and
/// declining (`n`) must leave `mine` exactly as it was.
#[gpui::test]
fn o_confirms_before_overwriting(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = services_with_a_saved_scope();
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
         [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
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

/// Task 5 review round 1, the Major: `o` on a scope the user layer does
/// not already own also forks it into the user layer (the same
/// consequence any other definitional edit through this dialog has), and
/// that has to be disclosed in the prompt *before* the second keystroke —
/// not discovered weeks later when the desk's changes stop arriving
/// (spec §16). `mine` here is builtin-owned only
/// (`services_with_a_saved_scope` — no user-layer `scopes` doc at all),
/// so `o` must arm `Confirm::Overwrite { forks: true }`, not `{ forks:
/// false }`. `overwrite_prompts_tell_the_truth_about_what_it_costs`
/// (`mod.rs`) pins that the `true` prompt's wording is honest once armed
/// this way; this test pins that `arm_overwrite` actually arms it this
/// way for a real desk-owned object, through the real dispatch path
/// (`editing_row`, not a hand-built `ObjectRow`).
#[gpui::test]
fn o_on_a_desk_owned_scope_discloses_the_fork_before_writing(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = services_with_a_saved_scope();
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::scopes");

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("o");
    cx.run_until_parked();

    assert_eq!(
        edit_draft(&shell, &cx, |draft| draft.confirm),
        Some(objectdialog::Confirm::Overwrite { forks: true }),
        "mine is builtin-owned, not user-owned, so o must disclose the fork"
    );
}

/// The non-forking twin: `o` on a scope the user layer already owns must
/// not claim it will fork anything.
#[gpui::test]
fn o_on_a_user_owned_scope_does_not_claim_a_fork(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = services_with_a_user_owned_scope();
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::scopes");

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("o");
    cx.run_until_parked();

    assert_eq!(
        edit_draft(&shell, &cx, |draft| draft.confirm),
        Some(objectdialog::Confirm::Overwrite { forks: false }),
        "mine is already user-owned, so o must not claim a fork"
    );
}

/// **A shell with nowhere to write must not move the draft's baseline.**
///
/// `commit_edit` resolves `ShellView::user_dir` *before* `mark_saved()`,
/// and this is the ordering that proves it: with no writable user config
/// directory nothing is queued, applied or written, so nothing has been
/// accounted for and the draft has to stay dirty. A `mark_saved()` ahead
/// of the check makes the unqueued value the baseline, and a later
/// declined `Confirm::Fork` then `revert_to_baseline`s onto a value that
/// was never applied and never persisted — the state `cancel_confirm`
/// exists to prevent. Every other fixture in this file has a user
/// directory, which is why this ordering regressed unseen.
#[gpui::test]
fn an_edit_with_nowhere_to_write_leaves_the_draft_dirty(cx: &mut gpui::TestAppContext) {
    // `dialog_test_shell_with`, not `..._in_dir`: this one's `user_dir` is
    // `None`.
    let (shell, mut cx) = dialog_test_shell_with(cx, services_with_a_desk_view(), "config::views");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("j j");
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
         [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
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

/// **A confirmed verb may not do visibly nothing.** `o` on a saved scope
/// that already equals the frame's — the ordinary state straight after
/// `:scope load mine`, not a corner — used to produce no write, no config
/// change and no message: the confirm row simply vanished after a
/// deliberate second keystroke. `commit_edit` answers `None` both for
/// "queued" and for "nothing changed", so the no-op is identified at the
/// call site instead.
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
    assert!(cx.debug_bounds("objectdialog-confirm").is_some());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        notice
            .as_deref()
            .is_some_and(|n| n.contains("already matches the frame")),
        "a confirmed o that writes nothing has to say why, got {notice:?}"
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

// ---------------------------------------------------------------------
// Task 5: `n` — the naming row, create, and the edit stage on a new
// object (§18.2).
// ---------------------------------------------------------------------

/// §18.2, Views: `n` opens the name field; `enter` on a valid name
/// writes the object, opens its edit stage, and the browse list has it.
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

/// Review round 1: `n` after a browse filter must not open the name
/// field pre-filled with the leftover query. `begin_naming` clears only
/// `state.query`; the shared `Input` is a second, separate buffer
/// (`set_value` does not emit the `Change` event that mirroring relies
/// on), and the natural sequence — filter to check whether a name is
/// taken, `escape` back to normal mode (which keeps the query applied),
/// then `n` — used to leave "tr" visibly sitting in a field `state.query`
/// no longer knew about. Typing `ee` into that stale text used to create
/// `tree` (an existing desk view, forked) instead of `ee`.
#[gpui::test]
fn n_opens_an_empty_name_field_even_after_a_browse_filter(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_a_desk_view(), dir.path(), "config::views");

    cx.simulate_keystrokes("/");
    cx.simulate_keystrokes("t r");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.query.clone()), "tr");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.query.clone()),
        "tr",
        "leaving filter mode keeps the query applied"
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

/// Scopes' `n` saves the FRAME's current scope, not an empty object —
/// the same read `run_confirmed`'s `Confirm::Overwrite` arm makes, made
/// here instead because there is no existing object's row to read it
/// from.
#[gpui::test]
fn n_on_a_scope_saves_the_frames_current_scope(cx: &mut gpui::TestAppContext) {
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
    assert!(
        written.contains("[today") && written.contains("BK007"),
        "{written}"
    );
    assert!(edit_draft(&shell, &cx, |d| d.is_new));
}

/// Groupings' nine slots are a fixed keyboard (§18.4) — there is nothing
/// `n` could create that is not already on the list, so it must say why
/// rather than silently doing nothing.
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

// ---- Groupings: digit jump and chain entry (§18.8) -----------------------

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

/// The review's Major: a digit jump away from a slot and back inside the
/// write debounce used to rebuild the slot's draft from `services.config`,
/// which the flush had not reached yet — the tick just made vanished from
/// the screen, and the stale draft then outlived the flush, so the NEXT
/// tick rendered the whole object without it and wrote that. The edit
/// stage now derives from the config with the pending batch folded in
/// (`apply::config_with_pending`), at the one door every entry goes
/// through, so the same holds for `escape` + `enter` re-entry.
#[gpui::test]
fn jumping_away_and_back_inside_the_debounce_keeps_the_queued_tick(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_slot_3(&["book"]),
        dir.path(),
        "config::groupings",
    );
    // Slot 1 is empty: ticking `book` queues a user-layer write with no
    // fork to confirm.
    cx.simulate_keystrokes("1 j j space");
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

    // And the stale-draft overwrite that followed: ticking `lhu` from
    // the re-entered stage must keep `book`.
    cx.simulate_keystrokes("j j j space");
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

/// The text the shared dialog `Input` currently holds.
fn dialog_input_text(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> String {
    shell.read_with(cx, |shell, cx| {
        shell.dialog_input.read(cx).text().to_string()
    })
}

/// The whole chain-field flow on a real window (§18.8): `i` opens the
/// field seeded with the slot's chain and hands it the keys, the row
/// list below becomes the completions for the segment being typed,
/// `tab` accepts the highlighted one, and `enter` makes the typed names
/// the chain — queued on the same batch a tick would be, reaching the
/// file behind the same debounce.
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
    assert!(edit_draft(&shell, &cx, |d| d.chain_entry));
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
    // No mouse verb while the field is open: a clicked `d`/`r` would arm
    // a confirm over a live, focused value field, which the keyboard can
    // never do (the review's Minor 3).
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
    assert!(!edit_draft(&shell, &cx, |d| d.chain_entry));
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
    // fork question a tick or a `shift+j` on it would ask
    // (`reordering_slot_3_and_pressing_ctrl_3_regroups_off_the_new_order`),
    // answered the same way.
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_some(),
        "a desk slot asks before forking, from the chain field too"
    );
    assert!(shell.read_with(&cx, |s, _| s.pending_config_write.is_none()));
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        shell.read_with(&cx, |s, _| s.pending_config_write.is_some()),
        "queued once the fork is confirmed, like a tick"
    );
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
    assert!(edit_draft(&shell, &cx, |d| d.chain_entry), "still open");
    assert!(dialog_filter_is_focused(&shell, &mut cx));
    assert_eq!(
        dialog_input_text(&shell, &cx),
        "book npv",
        "the text is intact"
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!edit_draft(&shell, &cx, |d| d.chain_entry));
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
    assert!(!edit_draft(&shell, &cx, |d| d.chain_entry));
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
    assert!(dialog_state(&shell, &cx, |s| s.notice.is_some()));
}

// ---- Task 6: filtering the edit stage (§18.3) -------------------------

/// `/` filters the edit stage's own rows, exactly as it does in browse:
/// the field labels are ranked against the query, a hidden row's element
/// does not paint, `escape` walks the whole ladder one visible rung at a
/// time (leave filter keeping the query, clear the query, back a stage),
/// and every verb along the way still acts on the row the trader is
/// actually looking at.
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

    // Leave filter, keep query.
    cx.simulate_keystrokes("escape");
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
    // The edit stage's own filter must not leak into the browse query:
    // `state.query` is a separate cursor space (§18.3), and the mirror
    // that fed `draft.query` while editing must never have touched it.
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

/// The mouse's half of §18.3's one switch. Clicking a row while the
/// edit stage is filtering must not silently take the keyboard back:
/// before §18.3 the edit stage could not be in `Filter` at all, so its
/// click handler focused the shell unconditionally, which after this
/// task left the pill reading `filter` and the caret painted while the
/// `Input` was blurred — every following keystroke went nowhere until
/// `escape`. Browse's own `on_row_clicked` already reads the mode; this
/// asserts the edit stage's does too.
#[gpui::test]
fn clicking_an_edit_row_while_filtering_keeps_the_filter_focused(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    cx.simulate_keystrokes("/");
    cx.run_until_parked();
    // "n" matches the `Columns` field label and the `npv` item, so the
    // click below lands on a row the filter is still showing.
    cx.simulate_input("n");
    cx.run_until_parked();
    let row = cx
        .debug_bounds("objectdialog-item-npv")
        .expect("npv matches the query and should paint");
    // Just inside the row's top edge rather than its centre: the
    // section header rides on this row's own element (§18.1), so the
    // row's box extends past the bottom of the scrolled list viewport
    // and a centre click would land outside it.
    cx.simulate_click(
        gpui::point(row.origin.x + gpui::px(8.0), row.origin.y + gpui::px(2.0)),
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();

    assert_eq!(
        edit_draft(&shell, &cx, |d| d.selected),
        1,
        "the click moved the cursor onto npv — the row it landed on"
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
        "np",
        "so the next character typed still reaches the filter"
    );
}

// ---------------------------------------------------------------------
// Task 8: the object dialog to the mock — crumb, badges, grip and tick,
// section headers (§18.1).
// ---------------------------------------------------------------------

#[gpui::test]
fn the_edit_stage_paints_section_headers_destination_badges_and_the_crumb(
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
    assert!(cx.debug_bounds("objectdialog-dest-dataset").is_some());
    assert!(cx.debug_bounds("objectdialog-dest-columns").is_some());
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

/// The name of the list item the edit-stage cursor is on — from either
/// list, the object's own or its available catalogue (§18.7.1) — or
/// `None` on a field row.
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

/// `space` promotes the row under the cursor into the member block and
/// leaves the cursor on the NEXT available row (user ruling 2026-09-11:
/// a trader adding several columns wants it there, not on the column
/// that just left). With the cursor on the last row the viewport shows,
/// that next row is one row past the viewport's bottom — `shift+j`
/// already scrolled the cursor back into view after a move, and this
/// verb moves it too, so without the scroll the cursor silently left
/// the viewport and the next `j` appeared to jump.
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
    // rows: Dataset=0, Columns=1, book=2, npv=3, m0=4 … so `m{last}` is
    // `last + 4` presses of `j` from the top.
    let presses = vec!["j"; last + 4].join(" ");
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

/// And `x` the other way round: the demoted row travels to the *end* of
/// the available block, a screenful below, but the cursor does not go
/// with it (user ruling 2026-09-11) — it stays at the top, on the row
/// that was next, which was on screen before the keystroke and still is.
/// That is also why the `x` arm no longer calls `scroll_to_cursor`: with
/// the cursor holding its own visible index there is nothing to scroll
/// to, and a call no test could see would be a harness lie.
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
    // Past `Dataset` and `Columns` onto `book`, the view's first member.
    cx.simulate_keystrokes("j j");
    cx.run_until_parked();
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

// ---------------------------------------------------------------------
// §16.1: the confirm buttons are the fourth way out of a stage, and the
// only one that reaches `run_confirmed` without passing through the key
// path — so they carry the sync themselves.
// ---------------------------------------------------------------------

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
