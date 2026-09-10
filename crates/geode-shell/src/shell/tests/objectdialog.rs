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
fn desk_view_docs() -> Vec<LayerDoc> {
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
/// fight the edit — and it does not, because it produces documents
/// identical to the ones memory already holds, so every `changed(..)`
/// predicate in `apply_reload` answers false and nothing is rebuilt,
/// re-emitted or closed.
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
/// alike.
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
