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
        // `npv` carries a desk `label` — the one column key Part 2c's
        // column stage can CLEAR (§5.3), and a clear is only meaningful
        // against a desk that set something. Nothing else here reads it;
        // it simply gives the stage a key to hand back.
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

/// **The mirror image of the test above.** Hiding a member is
/// presentation and says nothing; adding an AVAILABLE column changes what
/// the view IS, so it forks the desk view into the user layer — applied
/// at once and announced, like any other definitional edit (user ruling
/// 2026-09-14). `delta01` is on the dataset (`desk_view_docs`) but not
/// on `tree`'s own columns, so it is the one row in the available block
/// this fixture's `tree` has.
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

/// §19.6: the fork's own batch carries the overrides entry, so it lands
/// in the same flush; `r` removes it with the user copy.
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

/// §19.6, MINOR 10: `commit_edit`'s fork block inserts the stale
/// removals into the batch BEFORE its own fresh entry, and that order is
/// load-bearing. At fork time the user layer does not yet own the
/// object, so `stale_override_keys` reports the very key this fork is
/// about to write as stale — the ordinary case, not a rare collision.
/// Both inserts share one `BTreeMap` key, so whichever runs second wins;
/// this pins the fresh `Some` entry as the one that must.
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
    assert!(
        text.contains("[tree.columns.book]") && text.contains("hidden = true"),
        "{text}"
    );
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
        objectdialog::apply::finish_flush(shell, seq.wrapping_sub(1), Ok(()), None, cx);
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

/// A **definitional** change to an object the user's layer does not own
/// forks it into the user layer, and a fork freezes: the desk's next
/// column never reaches this trader (spec §4.1). It applies on the
/// keystroke like every other edit and is *announced* rather than asked
/// about (user ruling 2026-09-14: the confirm was "too distracting —
/// tell the user what is happening but just do it"): the notice names
/// the copy, the layer it shadows and the `r` that restores it, and the
/// write is queued before the keystroke returns.
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
    // Since a fork never asks (2026-09-14) the confirm's absence proves
    // nothing on its own — the fork is announced in the NOTICE now, so
    // that is where "an unconfigured slot forks nothing" has to be read.
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
    // layer does not own: it forks, applied at once and announced,
    // exactly like any other `Doc` edit to a desk/builtin object.
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

/// `o` on a scope the user layer does not already own forks it into the
/// user layer (the same consequence any other definitional edit through
/// this dialog has) and, by the 2026-09-14 ruling, does so at once and
/// says so — nothing is lost, the desk's copy is still there and `r`
/// restores it, so there is nothing to ask. `mine` here is builtin-owned
/// only (`services_with_a_saved_scope` — no user-layer `scopes` doc at
/// all), through the real dispatch path (`editing_row`, not a hand-built
/// `ObjectRow`). Task 5 review round 1's Major was that the fork went
/// undisclosed; the notice is where it is disclosed now.
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
    cx.simulate_keystrokes("j"); // onto the `book` item row
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
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("j");
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
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("j");
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
    cx.simulate_keystrokes("j"); // BK000
    cx.simulate_keystrokes("space"); // tick it
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
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("j");
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

/// `d`, `r` and `o` are none of them verbs while the Values stage is open
/// (scopes-editing spec §4) — each refuses with the same notice and
/// leaves the stage, the confirm and the draft untouched.
#[gpui::test]
fn d_r_and_o_refuse_inside_the_values_stage(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("j");
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

/// `space` on a SELECTED Scopes dimension row (the edit stage, not the
/// Values stage) names the door rather than opening it — only `enter`
/// does that (scopes-editing spec §3) — and asks the data for nothing.
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
    cx.simulate_keystrokes("j"); // the `book` item row
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

/// `x` on a selected Scopes dimension drops it outright: the saved
/// selection is removed (never written as `[]`) and the row moves to the
/// available block with no leftover note (review round 1's Important 1 —
/// `Draft::remove_selected` used to leave `entry.note` set, so a dropped
/// dimension's available row kept painting its old values' summary).
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
    cx.simulate_keystrokes("j"); // the `book` item row
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
    cx.simulate_keystrokes("j"); // `book`, the item row
    cx.simulate_keystrokes("j"); // `lhu`, the available row
    cx.simulate_keystrokes("x");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some("not selected — enter picks its values")
    );
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

/// Opening a slot lands in the chooser (user ruling 2026-09-14,
/// superseding §18.8's 2026-09-12 chain-field landing): normal mode, no
/// field open, the action bar up. `i` opens the chain field — seeded,
/// focused, completions below — and `escape` walks field → chooser →
/// browse, one visible rung at a time. The same holds by digit, by
/// `enter` and by a click (`clicking_a_groupings_row_opens_the_chooser`).
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
///
/// The row clicked is the `Columns` FIELD row, not the `npv` item row it
/// used to be: since dataset-presentation §4.1 a click on a member row
/// opens that column's stage (see
/// `clicking_a_member_row_opens_its_column_stage`), which sets normal
/// mode by design and so cannot also carry this test's question. A field
/// row opens nothing, which is what leaves the mode where the click found
/// it — the property under test. The cursor is stepped down onto `npv`
/// first (filter mode's own `down`, `listfilter::nav_command`), so
/// landing on row 0 is a move the click made rather than where it already
/// was.
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
    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.selected),
        1,
        "the cursor is on npv, so the click below is a real move"
    );
    let row = cx
        .debug_bounds("objectdialog-field-columns")
        .expect("the Columns field matches the query and should paint");
    // Just inside the row's top edge rather than its centre: the
    // section header rides on the following row's own element (§18.1), so
    // a row's box can extend past the bottom of the scrolled list
    // viewport and a centre click would land outside it.
    cx.simulate_click(
        gpui::point(row.origin.x + gpui::px(8.0), row.origin.y + gpui::px(2.0)),
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();

    assert_eq!(
        edit_draft(&shell, &cx, |d| d.selected),
        0,
        "the click moved the cursor onto Columns — the row it landed on"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "tree".to_string()
        },
        "a field row opens nothing"
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

/// The final whole-branch review's Minor 6: none of `d`, `r`, `o` is a
/// verb in a column's stage — all three refuse — so the action bar
/// advertises none of them there, on EITHER door. `tree` is overridden
/// in the user layer in this fixture, so both destructive buttons really
/// do paint one stage out; without the gate they paint here too, naming
/// the view (or, on the Schema door, the dataset) a keystroke can only
/// decline to delete.
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

/// Dataset-presentation §4.1's mouse-parity half on the VIEWS door: a
/// click on a member row does what `enter` would and opens that column's
/// stage. Before this, a member row was the one row in this dialog whose
/// `enter` did something a click would not (the Part 2c ledger's standing
/// minor).
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

// --- Mouse parity (interaction-model spec §17, 4c §18.9) ----------------

/// §17.1 rule 1 on the object dialog's browse stage.
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
}

/// And on the edit stage, whose frozen row is the draft's own (§18.3).
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

/// §17.1 rule 2 on browse: one click opens the row's edit stage through
/// the one door (`enter_edit_stage`), exactly as `enter` does.
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

/// On Groupings a click lands in the chooser like every other door
/// (user ruling 2026-09-14): the door decides, not the click, and the
/// door opens no field.
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

/// While the naming row is open a click only selects: a typed name must
/// not be discarded by a stray click, and `enter` there creates.
///
/// The typed text is `wd`, not `mine` and not `wide` itself. The browse
/// list underneath is deliberately still ranked by the naming text
/// (CLAUDE.md's Phase 4c Part 2a paragraph — "so a near-collision stays
/// visible before `enter` refuses it"), and the ranker is a *subsequence*
/// matcher, so `mine` — no fuzzy match against this fixture's `tree` or
/// `wide` — would leave no row to click at all, exercising the ranking
/// rule instead of the click rule this test is about. But `wide` itself
/// would be just as wrong the other way: it is indistinguishable from
/// `clicked`, so a slip that let the non-opening branch write
/// `state.query = name.clone()` instead of leaving it untouched would
/// still read back `wide` and this test would not catch it. `wd` is a
/// genuine subsequence of `wide` (keeping the row visible) while
/// differing from it, so only "the click left the typed text alone"
/// makes the assertion below pass.
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

/// §18.9.2: the tick is the toggle. Clicking a shown column's tick hides
/// it — a `Presentation` write, no fork question — and leaves the cursor
/// on that row, as `space` would.
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

/// §17.1 rule 3 / §18.9.2 follow-up: a tick click is claimed and dropped
/// while a confirm is armed, exactly as a bare letter is on the key path
/// (`handle_edit_key`'s own `armed` block). `open_tree_edit_stage`'s
/// fixture cannot arm `Confirm::Delete` — `tree` is desk-owned, so `d`
/// there sets a notice pointing at `r` instead (see
/// `arm_delete`'s `Layer::User` gate) — so this reuses
/// `services_with_a_user_only_view`, the same fixture the neighbouring
/// `deleting_a_user_layer_object_leaves_the_browse_list_before_the_watcher_could_fire`
/// test arms `d` against, where the "mine" object IS the user's own.
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

/// §18.9.3 at the shell level: the drop handler reorders, parks the
/// cursor on the dropped item and queues the presentation write.
///
/// This is the lowest rung the gesture can be tested on, and §18.9.5
/// says why: gpui's own drag machinery does not run under
/// `TestAppContext`. A `simulate_mouse_down` on a row followed by a move
/// well past `DRAG_THRESHOLD` never leaves `App::has_active_drag` set —
/// verified here on 2026-09-12, with and without an intervening
/// `window.draw`, and with a hover move before the press — so a test of
/// the full `on_drag` → `on_drop` path would assert nothing about this
/// dialog and everything about the harness. The wiring above this call
/// (`row_drag` into `on_drag`, `can_drop`, `drag_over`, `on_drop`) is a
/// display-check item alongside §18.6's; everything below it is tested
/// here and in `Draft::drop_row`'s own tests.
#[gpui::test]
fn the_drop_handler_reorders_and_writes(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    // The cursor is parked on the TARGET, not the dragged row (`j`
    // moves it off `book` and onto `npv`), which is what makes this
    // test able to see §18.9.1's rule at all: a handler that read the
    // source off the cursor instead of the payload would drop `npv`
    // onto itself and leave the list exactly as it found it.
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

/// §17.1 rule 3 / §18.9.3: a drop is claimed and dropped while a confirm
/// is armed, exactly as `on_tick_clicked` is and as a bare letter is on
/// the key path. The same fixture reasoning as
/// `a_tick_click_does_nothing_while_a_confirm_is_armed`: only a
/// user-layer object can arm `Confirm::Delete`, so `tree` (desk-owned)
/// cannot be used here.
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

/// §18.9.4: clicking a completion row is the mouse form of `tab` — the
/// trailing segment is replaced by that row and the next opened with
/// ` / `; the field stays focused and the pill still reads `chain`.
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

/// [`services_with_a_desk_view`]'s desk, with a SECOND measure the view
/// does not carry, so `tree`'s available block has two rows rather than
/// one.
///
/// A catalogue-to-catalogue drop needs two DISTINCT `Available`
/// payloads, which the shared fixture (one spare column, `delta01`)
/// cannot produce: dropping its only available row on itself is a
/// self-drop, which is silent by ruling and so would test the opposite
/// of what this arm says. The dataset is restated by name rather than by
/// index into `desk_view_docs()` so a doc added there cannot silently
/// make this fixture overwrite the wrong one.
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

/// §18.9.3: a drop from the catalogue onto the catalogue says so — the
/// catalogue is unordered by construction (§18.7.2), so there is nothing
/// for the gesture to have done, and a trader who just dragged one
/// available column onto another would otherwise have no way to tell
/// that from the app having missed the drop.
///
/// The second half is the controller's M5 ruling: the same gesture ONTO
/// ITSELF is a grab that went nowhere and stays silent, which is also
/// why it has to be decided before the catalogue arm — two identical
/// available payloads satisfy that arm's `!src.own && !dst.own` test
/// too.
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

    // M5: the same row on itself is silent — and clears the notice the
    // previous drop left, the way every handler here starts.
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

/// §18.9.1: the payload is resolved by name at drop time, so a name that
/// left the list between the grab and the drop lands on nothing — and
/// says so, because a drag that visibly ended over a row and changed
/// nothing is the one inert case a trader would read as a bug rather
/// than as their own gesture.
///
/// The stale payload is hand-built rather than staged through a real
/// removal: `RowDrag` is what crosses the wire, and a name that no row
/// carries is exactly what a mid-drag removal leaves in flight.
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

/// §19.4: the inspector lists datasets, opens one to its column rows —
/// each with the layer it came from — and refuses every verb with one
/// notice; `n` is refused in browse and the footer never offers it.
///
/// `enter` is NOT in the refused list any more (dataset-presentation spec
/// §4.1): on a column row it opens that column's stage, which is this
/// dialog's one writable surface. Spec §1.3's done state names exactly
/// which verbs still answer the read-only notice on these rows — `d`,
/// `r`, `n`, ticks and drops — and `enter` is not among them.
/// `the_schema_column_row_opens_the_column_stage_and_writes_the_dataset_overlay`
/// is where that door is asserted, and it presses `d` again after
/// `escape` so this dialog's own rows are still proved read-only once the
/// stage has been in and out.
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

/// Spec §20.3 on the read-only Schema domain: no `n` button on browse
/// (ruling 6 — a button that only ever refuses teaches a verb with
/// nothing behind it), and the chip's own door refuses with the read-only
/// notice and changes nothing.
///
/// The chip half is a direct call, not a click: every Schema edit-stage
/// row is a display-only `Text`, so `vocabulary_of` answers `Inert` and
/// no Schema row ever paints a chip whatever `chips_live` says — the
/// render gate is unobservable here by construction. What IS observable
/// is `on_value_chip_clicked`'s own writable gate, which is the one a
/// future steppable Schema row (or a test) would reach.
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

/// Dataset-presentation spec §4: `enter` on a schema column row opens
/// the column stage crumbed `risk › book`; `i` types a width that lands
/// under `[risk.columns.book]` and NOWHERE else; `d`/`r` are refused
/// inside the stage; the row shows the summary after; the schema rows'
/// own verbs still answer read-only.
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
    // §4.6: the two destructive verbs are refused in a column's stage —
    // through `in_column_stage`, not the read-only gate, so the wording
    // is the column stage's own and identical to the Views door's.
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
    // §4.5 and `schema::to_table`'s branch: the write is the overlay and
    // only the overlay — a `dest`-blind `to_table` would have rendered
    // the whole `datasets` object into it, and a stage that forgot its
    // destination would have forked the schema into the user layer.
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

/// The final whole-branch review's Minor 4: `maybe_refresh_available`
/// seeds each catalogue row from `dataset_presentation.toml` (§5.4), so
/// it must read the config with the PENDING batch folded in. Inside the
/// 250 ms debounce — a Schema column-stage edit, out of that dialog,
/// into Views, step the `dataset` row — a plain `services.config` read
/// is the overlay as it stood before the last keystroke, and a column
/// promoted off that stale catalogue carries the stale layer into the
/// writer's comparison.
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

/// §4.1 mouse parity: a click on a schema column row opens the stage.
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

/// §4.7: `escape` out of a column stage puts the cursor back on the
/// column's OWN row, not at the top of a thirty-column list — the Schema
/// door's mirror of the Views door's `select_item_named`. `position_ref`
/// is the second row, so a cursor that merely reset to zero fails here.
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

/// Review round 1's Important: `on_tick_clicked` and `on_row_dropped`
/// are the mouse's own paths to the same writes the keyboard gate above
/// refuses, and `Domain::writable`'s own doc comment names both as gate
/// sites — a mouse drop on a read-only row must refuse identically to a
/// keystroke, not merely fail to find anything to drag. `on_row_dropped`
/// is `pub(in crate::shell)` precisely so this can drive it directly,
/// the same door `a_drop_whose_name_has_left_the_list_says_that_row_is_gone`
/// above uses.
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

/// A two-source `sources.toml` over a two-dataset schema — `vols` feeds
/// `vol`, `live` feeds `risk` and carries every optional key, so the
/// window tests below have both the sort order (§19.3: dataset first)
/// and a full set of fields to exercise `i` against.
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

/// §19.3: rows read dataset first and sort by it; `i` on a text row
/// opens the field seeded with the value; a bad duration is refused with
/// the field open; a good one applies and, on a builtin source, forks it
/// without asking; the flush writes the spelling the reader reads.
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
    // §19.3's delivery path: the in-memory apply ran `apply_reload`, whose
    // sources-baseline comparison raised the existing stripe.
    assert!(
        shell
            .read_with(&cx, |s, _| s.restart_required.clone())
            .is_some_and(|m| m.contains("sources")),
        "a sources write raises the restart-required stripe"
    );
}

/// §19.3: `n` seeds the dataset from the cursor row and the name from
/// it when free; the created source is idle (empty paths, a warning
/// on the row, never an error).
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

// --- Task 4: `Diagnostic.path` lands on its field row (4c §19.5) --------

/// §19.5: a reader diagnostic that names a column lands on that column's
/// row as a glyph, and its header line is prefixed with the row's label;
/// an object-level one — here, a join missing `dataset`, whose path
/// (`views.tree.joins`) names a key no `Field` owns and so cannot
/// resolve to any row — stays on the header alone, with no glyph
/// anywhere and no prefix on its own header line.
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
    // Review round 1's Important-1 finding: the glyph used to be a THIRD
    // direct child of the row under `justify_between`, which splits the
    // row's free space into two gaps and floats the label toward the
    // row's centre — on every row, flagged or not, since neither the
    // glyph nor the label carries `flex_1()`. Comparing the flagged
    // row's label origin against the unflagged row's is what would have
    // caught that: with the bug, delta's label (flagged, three children)
    // sits at a different x than npv's (unflagged, two children); fixed,
    // both labels start at the same x regardless of the glyph's content.
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

/// Review round 1's Important-2 finding: `views.toml` declares `npv`
/// first and `delta` second (so the reader's diagnostic index — a
/// position in THAT order — names `delta` at index 1), but a
/// `view_presentation.toml` `order` flips them to `delta` first for the
/// edit stage's `items`. The glyph must still land on `delta` — the
/// column the diagnostic actually names — not on whatever the raw index
/// now happens to point at in the reordered `items` (`npv`, the bug this
/// finding describes).
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
    // `items` is now `[delta, npv]` (the presentation's order), so a
    // pre-fix raw-index lookup of `columns.1` would have landed on
    // `npv` — this is the assertion that would have failed before the
    // fix.
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

/// §19.6: a flush whose in-memory merge is refused still writes the file
/// (disk stays the arbiter), and the status line says both halves.
#[gpui::test]
fn a_flush_the_merge_rejects_says_saved_but_rejected(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    // `keymap.mod = "ctrl"` is refused with an ERROR diagnostic at every
    // reload (Phase 4a Task 4b), so any batch applied over this user
    // layer is rejected while its own object is fine.
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

/// The edit footer names `i` on the row that can take it and on no
/// other (user ruling 2026-09-13, narrowing the 2026-09-12 rule from the
/// object to the row): Sources' `Paths` is an editable `Text`, but the
/// `Dataset` row the stage opens on is a `Choice` that `i` refuses.
#[gpui::test]
fn the_edit_footer_offers_i_only_where_a_row_can_take_it(cx: &mut gpui::TestAppContext) {
    let (_shell, mut cx) = dialog_test_shell_with(cx, services_with_sources(), "config::sources");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-hint-i").is_none(),
        "Sources opens on Dataset, a Choice i cannot open"
    );
    assert!(
        cx.debug_bounds("objectdialog-hint-change").is_some(),
        "which the step keys do change"
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

// ---- Part 2c Task 4: the column stage (2c §5) -------------------------

/// §5: enter on a member opens the column stage; a step there writes
/// one [view.columns.<col>] key to the overlay and never forks the view;
/// escape returns to the view's stage with the cursor on the column.
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

    // §5.3's clear verb, end to end: `i` on `Label`, typed empty,
    // `enter`. An empty label means "stop overriding", never "delete" —
    // this overlay cannot remove a key `views.toml` sets — so the field
    // comes back reading the desk's own label, the trader is told why,
    // and no `label` key reaches the file.
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

    // And `enter` means the same thing in filter mode, which is how a
    // trader reaches one column of a thirty-column view.
    cx.simulate_keystrokes("/");
    cx.simulate_input("npv");
    cx.run_until_parked();
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

/// Dataset-presentation spec §5: the Views column stage with a layer
/// under it. All three layers are filled by the door
/// (`views::column_layers`), so the label a trader sees is the DATASET's
/// (which beats the desk's own `NPV`), clearing it says it follows the
/// dataset rather than the desk, the field re-seeds from that layer, and
/// no `label` key reaches `view_presentation.toml` — the trader's own
/// dataset-level opinion is not copied into this one view.
///
/// The provenance chip is asserted by existence (a `debug_bounds` id
/// carries no text): `scale`, set at the dataset level, paints one;
/// `width`, set at no layer and never touched, paints none. Which layer
/// each chip NAMES is the pure half — `dataset_columns::provenance_of`'s
/// own tests — over the layers this test proves the door fills.
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
    // `open_tree_edit_stage`'s own `j j` (past Dataset and Columns, onto
    // `book`), then one more onto `npv` — the column the dataset overlay
    // above speaks for.
    cx.simulate_keystrokes("j j j enter");
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |s, _| objectdialog::render::crumb_text(s)),
        "tree › npv"
    );

    // The door filled both layers below the view overlay, each as the
    // keys that layer itself sets — the desk's own label is still
    // nameable underneath the dataset's, which is what lets the fold
    // notice choose. The view overlay is not captured at all: the chip
    // asks whether the field differs from these two (the final
    // whole-branch review's named risk 4).
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

/// §5.2: a failed write rebuilds the draft from the reverted config —
/// the OBJECT's fields, with no column projection on them — so the stage
/// has to come back with it rather than leaving the crumb naming a column
/// whose seven fields are no longer installed.
///
/// Same fixture trick `a_failed_removal_reverts_the_in_memory_change_and_
/// says_so` uses: the file on disk will not parse, but the config in
/// memory never read it, so only the write can discover the problem.
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

/// I-2 (Part 2c final review): `d` and `r` are refused in the column
/// stage, through the same `not_a_column_verb` notice `x`, `shift+j` and
/// `shift+k` already answer with.
///
/// The crumb has narrowed the object to one column, and both verbs act
/// on the WHOLE view — `d` deletes the user-layer view, `r` undoes the
/// trader's personalisation of every column of it, not the open one. The
/// fixture makes that reachable rather than merely refused-anyway: one
/// `space` on `scale` gives the view a user-layer presentation override,
/// so `r` would arm a real `Confirm::Revert` here absent the guard.
///
/// The last block is the scoping half: the refusal is the column
/// stage's, not a blanket disabling of the two letters — one `escape`
/// back to the view and `r` arms exactly as it always did.
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

/// §5.3: `width` is a typed value, not a stepped one — `i` opens it
/// seeded with `auto`, a pixel count inside the range applies and reaches
/// the overlay, and anything else is refused with the range named and the
/// field still open on the trader's own text.
#[gpui::test]
fn the_column_stages_width_is_typed_and_refused_out_of_range(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    cx.simulate_keystrokes("j enter"); // npv's column stage
    cx.run_until_parked();
    cx.simulate_keystrokes("j"); // label → width
    cx.run_until_parked();
    // §5.2: the list verbs answer about THIS stage, not about a column
    // list that is not on screen — one sentence, whichever key.
    for (key, pressed) in [("x", "x"), ("shift-j", "shift+j"), ("shift-k", "shift+k")] {
        cx.simulate_keystrokes(key);
        cx.run_until_parked();
        assert_eq!(
            dialog_state(&shell, &cx, |s| s.notice.clone()),
            Some(format!("{pressed} is not a verb in a column's stage"))
        );
    }
    // `enter` names the verb this row actually has — `i`, not `space`,
    // and certainly not "read-only", which is what an editable `Text`
    // used to be told it was.
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
    // Typing mirrors through the `Input`'s change subscription; the
    // cursor must still be on the width row (1), not reset to the top as a
    // filter keystroke is (found on a display 2026-09-13).
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

// --- Task 5: the Colours dialog (Part 2c §6.1) --------------------------

/// A builtin `colours` doc with one colour (`delta`, `hue = 240`) plus
/// the keymap — the same shape `services_with_sources` uses, so a first
/// edit to `delta` forks it exactly as a builtin source's first edit
/// does.
fn services_with_colours() -> ShellServices {
    let mut services = test_services();
    let colours = LayerDoc::builtin("colours", "[delta]\nhue = 240\n").unwrap();
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

/// §6.1: the browse rows and the edit header carry a swatch resolved
/// against the active theme; stepping the hue repaints it; `n` refuses a
/// reserved name.
#[gpui::test]
fn the_colours_dialog_paints_swatches_and_refuses_reserved_names(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_colours(), dir.path(), "config::colours");
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
    let written = std::fs::read_to_string(dir.path().join("colours.toml")).unwrap();
    assert!(written.contains("[delta]\nhue = 255"), "{written}");
}

// --- Spec §20.3 / §20.6: the value chip, the i/n buttons, the armed guard --

/// Spec §20.3 on the object dialog: the hue chip steps on click and
/// shift+click through `step_selected_row`'s own path (so it writes), a
/// click on the row's label only selects, and the chip is plain text —
/// no handler — while a confirm is armed.
#[gpui::test]
fn the_value_chip_steps_a_number_and_is_inert_under_a_confirm(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_colours(), dir.path(), "config::colours");
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
    let written = std::fs::read_to_string(dir.path().join("colours.toml")).unwrap();
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

/// The two keyboard-only verbs gain buttons: `i` on the edit stage's bar
/// when the selected row is one `i` opens, `n` on the browse stage.
#[gpui::test]
fn i_and_n_have_buttons_that_do_what_their_keys_do(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_colours(), dir.path(), "config::colours");
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
    cx.simulate_keystrokes("escape j"); // tone, a Choice: steps but never types
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-action-i").is_none(),
        "i is not offered on a row it cannot open"
    );
}

/// §20.6's fallout: an edit-row click while a confirm is armed is claimed
/// and dropped, like the tick click — it neither moves the cursor nor
/// opens a column stage that would silently disarm the question.
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
    cx.simulate_keystrokes("enter j"); // mine, cursor on the `Columns` row
    cx.run_until_parked();
    let before = edit_draft(&shell, &cx, |d| d.selected);
    assert_eq!(before, 1);
    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-confirm").is_some());

    // A plain field row: the cursor stays where it was.
    let row = cx
        .debug_bounds("objectdialog-field-dataset")
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

    // The frozen filter row (the mouse form of `/`, §17.1 rule 1) is
    // dropped too: the question owns the mouse until it is answered.
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

/// The `n` button carries §19.3's Sources seeding exactly as the key
/// does — the dataset and the name field both from the row under the
/// cursor — read at click time, not baked into the button at paint.
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

/// Groupings is the one domain whose `i` reaches past the selected row
/// to the slot's whole chain (§18.8), so its button is live on every row
/// of a slot's chooser and opens the CHAIN field — the key's own door,
/// not a per-row value field — and browse offers no `n` on a fixed
/// roster, exactly as its footer names none.
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

/// `enter` is named wherever it opens something and nowhere else (user
/// ruling 2026-09-13): browse in both modes (it opens the edit stage),
/// the Views edit stage (a member row opens its column stage), the
/// Schema edit stage (a column row opens the dataset-level stage) — and
/// not inside a column stage, where `enter` on a field only gives a
/// notice.
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
        cx.debug_bounds("objectdialog-hint-enter").is_none(),
        "the Views edit stage lands on the dataset row, where enter only gives a notice"
    );
    cx.simulate_keystrokes("j j");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-hint-enter").is_some(),
        "on a member row enter opens its column stage"
    );
    cx.simulate_keystrokes("enter");
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

// ---- Step keys and the row-sensitive footer (user ruling 2026-09-13) ---

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

/// User ruling 2026-09-13 ("I keep reaching for them"): `l`/`h` step the
/// row under the cursor forward and back in normal mode, exactly as
/// `space`/`shift+space` do — one path, `Draft::step_selected`, so the
/// aliases cannot drift from the keys they alias.
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

    // An inert row names the key the trader actually pressed (review
    // 2026-09-13): `space` TYPES in filter mode, so "nothing on this row
    // changes with space" would be a sentence about a key that puts a
    // character in the query.
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

/// The footer names the keys the row under the CURSOR answers to, not
/// the ones its domain has somewhere (user ruling 2026-09-13). The
/// column stage is where all three cases sit side by side: `label` is an
/// editable `Text` (`i` only), `scale` a `Choice` (the step keys only),
/// `precision` a `Number` (both).
#[gpui::test]
fn the_edit_footer_names_only_what_the_selected_row_offers(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    // The reorder group is row-sensitive for the same reason (review
    // 2026-09-13): `shift+j`/`shift+k` move a list ITEM, and this fixture
    // lands on one.
    assert!(
        cx.debug_bounds("objectdialog-hint-reorder").is_some(),
        "a member row can be reordered"
    );
    cx.simulate_keystrokes("k k"); // the Columns header, then the Dataset row
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-hint-reorder").is_none(),
        "the dataset row has no item to move, so shift+j/shift+k must not be named"
    );
    cx.simulate_keystrokes("j j"); // back to the member row

    cx.simulate_keystrokes("j enter"); // npv's column stage, cursor on `label`
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
        cx.debug_bounds("objectdialog-hint-i").is_none(),
        "and i has nothing to open on a Choice"
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

/// Scopes-editing spec §3.2 (ruling 4): a scope's own dimensions list is
/// unreorderable, so the footer offers no `shift+j`/`shift+k` chip on its
/// item row even though `vocabulary_of` answers `RowVocabulary::Item` for
/// it exactly as any other list item does — the domain exclusion this
/// task adds to `reorders` (`render.rs`) is what keeps that chip off,
/// not the row's own vocabulary.
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
    cx.simulate_keystrokes("j"); // the `book` item row
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-hint-reorder").is_none(),
        "a scope's selections have no order, so the reorder chip must not paint"
    );
}

/// A click on an available dimension's tick opens its Values stage
/// exactly as `space` does from the keyboard (`on_tick_clicked` walks
/// [`step_selected_row`] itself, scopes-editing spec §3) — without that,
/// the click would fall straight to `Draft::toggle_selected` and add an
/// empty selection instead.
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

// --- Delete and revert from the browse list (user request 2026-09-19) ---

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

/// A read-only domain refuses the browse `d`/`r` through the same gate
/// every other verb goes through (§19.4), rather than silently dropping
/// the key as browse used to.
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

/// While a browse confirm is armed the question owns the keys AND the
/// mouse (spec §20.1): a row click neither opens the row nor moves the
/// target out from under the question.
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

/// Spec §20.3: the browse bar offers `d` and `r` as buttons beside `n`,
/// each only while the SELECTED row makes it live — the edit bar's own
/// gates — and the button arms exactly as the key does.
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
    cx.simulate_keystrokes("/ m i n e escape");
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

/// Review finding (2026-09-19): the removal is applied to memory by a
/// spawned task, AFTER `run_confirmed` returns, so a landing that reads
/// `services.config` clamps against the list as it stood BEFORE the
/// delete — a no-op. Deleting the LAST row from browse then left
/// `selected` one past the end: no row highlighted, no bar, `enter`
/// answering "no object is selected" with rows plainly on screen. The
/// landing has to read the pending-aware config
/// (`apply::config_with_pending`), the fold `enter_edit_stage` already
/// uses for the same reason.
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

/// Review finding (2026-09-19): the browse cursor is an INDEX, and a
/// config reload landing between `d` and `enter` (a desk push, an
/// external editor) re-ranks the list under the question — so the
/// prompt named one object and the answer removed another. The name is
/// recorded when the question is armed, and an answer whose target no
/// longer matches it is refused with a notice rather than carried out.
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
    cx.simulate_keystrokes("/ m i n e escape d");
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

/// Re-review minor (2026-09-19): one verb, one landing. A delete from the
/// EDIT stage lands the cursor on the deleted row's neighbour exactly as
/// a browse delete does — not on row 0, which is where a by-name lookup
/// against the pending-aware rows (the name is gone) fell through to.
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

/// Re-review minor (2026-09-19): the armed prompt names the object the
/// question was RECORDED for, not whatever the index resolves to on the
/// current frame — after a reload re-ranks the list under it, the prompt
/// and the refusal agree about which object was asked about.
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

// --- Field help: one context-sensitive line per selected row (2026-09-19) ---

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
        ("config::colours", services_with_colours, "token"),
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

/// User report 2026-09-19: moving the cursor from an editable row to a
/// read-only one dropped the footer's edit row, so the dialog shrank by a
/// line and everything below it shifted. Every hint row is now laid out
/// every time — an empty one painted blank at the same height — so the
/// go row sits at one y whichever row is selected.
#[gpui::test]
fn the_footer_keeps_its_rows_when_the_selected_row_has_nothing_to_edit(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_sources(), dir.path(), "config::sources");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    // `dataset` is a `Choice`: the edit row names the step keys.
    assert!(cx.debug_bounds("objectdialog-hint-change").is_some());
    let go_before = cx.debug_bounds("hint-row-go").unwrap();
    let edit_before = cx.debug_bounds("hint-row-edit").unwrap();

    // `k` wraps to the last row, `adapter`, a read-only `Text`: nothing
    // to edit, so the edit row is empty — but still there, same height.
    cx.simulate_keystrokes("k");
    cx.run_until_parked();
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
    assert_eq!(go.origin.y, go_before.origin.y, "so nothing below it moves");
}
