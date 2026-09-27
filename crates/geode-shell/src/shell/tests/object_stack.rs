//! Object dialogs of different domains stack over each other. The covered one is
//! parked in its own stack entry and comes back exactly as it was left; the same
//! domain never nests.

use super::objectdialog::{
    desk_view_services, dialog_state, edit_draft, flush_config_write, open_expression_field,
    services_with_a_saved_scope,
};
use super::*;
use crate::shell::dialog::DialogKind;
use crate::shell::objectdialog::{self, Domain};
use crate::shell::{EXPR_KEY, SCOPES_KEY};
use geode_core::query::DistinctOutcome;

fn kinds(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> Vec<DialogKind> {
    shell.read_with(cx, |s, _| s.modals.iter().map(|m| m.kind).collect())
}

/// The domain of every object dialog in the stack, bottom first: parked ones from
/// their entries, then the live one.
fn domains(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> Vec<Domain> {
    shell.read_with(cx, |s, _| {
        s.modals
            .iter()
            .filter_map(|m| m.parked_object.as_ref().map(|p| p.state.domain))
            .chain(s.object_dialog.as_ref().map(|d| d.domain))
            .collect()
    })
}

/// Open the palette, type `title`, press Enter: the trader's route.
fn open_palette_action(cx: &mut gpui::VisualTestContext, title: &str) {
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input(title);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
}

fn views_with_colors() -> ShellServices {
    desk_view_services(&[("colors", "[delta]\nhue = 240\n")])
}

/// The Views dialog on `tree`'s edit stage, over caller-supplied services.
fn open_tree_edit_stage_with(
    cx: &mut gpui::TestAppContext,
    services: ShellServices,
    dir: &std::path::Path,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir, "config::views");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    (shell, cx)
}

/// Views in a column stage, Colors pushed from the palette, then Escape back:
/// Views returns on the same column stage and row.
#[gpui::test]
fn colors_pushes_over_a_views_column_stage_and_escape_returns_to_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage_with(cx, views_with_colors(), dir.path());
    cx.simulate_keystrokes("enter"); // first column's stage
    cx.run_until_parked();
    let stage = dialog_state(&shell, &cx, |s| s.stage.clone());
    assert!(
        matches!(stage, objectdialog::Stage::Column { .. }),
        "{stage:?}"
    );
    cx.simulate_keystrokes("j j");
    let selected = edit_draft(&shell, &cx, |d| d.selected);

    open_palette_action(&mut cx, "Edit colors");
    assert_eq!(
        kinds(&shell, &cx),
        vec![DialogKind::Object, DialogKind::Object]
    );
    assert_eq!(domains(&shell, &cx), vec![Domain::Views, Domain::Colors]);
    assert!(
        cx.debug_bounds("objectdialog-swatch-delta").is_some(),
        "Colors paints"
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(kinds(&shell, &cx), vec![DialogKind::Object]);
    assert_eq!(domains(&shell, &cx), vec![Domain::Views]);
    assert_eq!(dialog_state(&shell, &cx, |s| s.stage.clone()), stage);
    assert_eq!(edit_draft(&shell, &cx, |d| d.selected), selected);
}

/// The same domain never nests: Views over Views is refused with a notice naming
/// the domain, and the live state is untouched.
#[gpui::test]
fn the_same_domain_is_refused_with_its_own_notice(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage_with(cx, views_with_colors(), dir.path());
    open_palette_action(&mut cx, "Edit colors");
    // Dispatch directly for the refusals: the notice is read before any later key
    // could replace it.
    dispatch_action(&shell, "config::views", &mut cx);
    assert_eq!(domains(&shell, &cx), vec![Domain::Views, Domain::Colors]);
    assert_eq!(
        shell.read_with(&cx, |s, _| s.notice),
        Some("views is already open underneath")
    );
    // Asking for the domain already on top does nothing and says nothing.
    shell.update(&mut cx, |s, _| s.notice = None);
    dispatch_action(&shell, "config::colors", &mut cx);
    assert_eq!(domains(&shell, &cx), vec![Domain::Views, Domain::Colors]);
    assert_eq!(shell.read_with(&cx, |s, _| s.notice), None);
}

/// Three deep: each pop reveals its own dialog, and a covered domain cannot be
/// requested again from the top.
#[gpui::test]
fn three_object_dialogs_pop_in_order(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage_with(cx, views_with_colors(), dir.path());
    open_palette_action(&mut cx, "Edit colors");
    cx.simulate_keystrokes("enter"); // delta's edit stage
    cx.run_until_parked();
    open_palette_action(&mut cx, "Edit scopes");
    assert_eq!(
        domains(&shell, &cx),
        vec![Domain::Views, Domain::Colors, Domain::Scopes]
    );
    dispatch_action(&shell, "config::colors", &mut cx);
    assert_eq!(
        domains(&shell, &cx),
        vec![Domain::Views, Domain::Colors, Domain::Scopes]
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s.notice),
        Some("colors is already open underneath")
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(domains(&shell, &cx), vec![Domain::Views, Domain::Colors]);
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "delta".into()
        },
        "Colors comes back on delta's edit stage"
    );
    cx.simulate_keystrokes("escape escape");
    cx.run_until_parked();
    assert_eq!(domains(&shell, &cx), vec![Domain::Views]);
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit { .. }
    ));
}

/// A non-object dialog between two object dialogs: the Views state is parked with
/// the Views entry, not the Choice list, and survives both pops.
#[gpui::test]
fn a_choice_list_between_two_object_dialogs_keeps_the_lower_one(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage_with(cx, views_with_colors(), dir.path());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let stage = dialog_state(&shell, &cx, |s| s.stage.clone());
    dispatch_action(&shell, "log::level", &mut cx);
    dispatch_action(&shell, "config::colors", &mut cx);
    assert_eq!(
        kinds(&shell, &cx),
        vec![DialogKind::Object, DialogKind::Choice, DialogKind::Object]
    );
    assert!(shell.read_with(&cx, |s, _| s.modals[0].parked_object.is_some()));
    assert!(shell.read_with(&cx, |s, _| s.modals[1].parked_object.is_none()));

    cx.simulate_keystrokes("escape"); // Colors
    cx.run_until_parked();
    assert_eq!(
        kinds(&shell, &cx),
        vec![DialogKind::Object, DialogKind::Choice]
    );
    assert_eq!(
        domains(&shell, &cx),
        vec![Domain::Views],
        "Views is live again, covered by the Choice list"
    );
    cx.simulate_keystrokes("escape"); // the Choice list
    cx.run_until_parked();
    assert_eq!(kinds(&shell, &cx), vec![DialogKind::Object]);
    assert_eq!(dialog_state(&shell, &cx, |s| s.stage.clone()), stage);
}

/// A failed Colors write while Views is covered rebuilds only Colors' draft. Views
/// comes back on its column stage: a rebuild would have dropped it to the edit stage
/// and discarded the trader's place.
#[gpui::test]
fn a_failed_stacked_write_leaves_the_covered_draft_alone(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage_with(cx, views_with_colors(), dir.path());
    cx.simulate_keystrokes("enter"); // column stage
    cx.run_until_parked();
    let stage = dialog_state(&shell, &cx, |s| s.stage.clone());
    // Unparseable and never loaded: only the write discovers it.
    std::fs::write(dir.path().join("colors.toml"), "[delta\nhue =").unwrap();

    open_palette_action(&mut cx, "Edit colors");
    cx.simulate_keystrokes("enter space"); // delta, hue step
    cx.run_until_parked();
    flush_config_write(&mut cx);
    assert!(
        dialog_state(&shell, &cx, |s| s.notice.clone())
            .is_some_and(|n| n.contains("could not save")),
        "Colors says its write failed"
    );

    cx.simulate_keystrokes("escape escape"); // edit stage → browse → close
    cx.run_until_parked();
    assert_eq!(domains(&shell, &cx), vec![Domain::Views]);
    assert_eq!(dialog_state(&shell, &cx, |s| s.stage.clone()), stage);
    assert_eq!(dialog_state(&shell, &cx, |s| s.notice.clone()), None);
}

/// Both dialogs contributed to one failed batch: both in-memory edits were reverted,
/// so both drafts are rebuilt to show it.
#[gpui::test]
fn a_shared_batch_failure_rebuilds_every_contributing_draft(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage_with(cx, views_with_colors(), dir.path());
    // A Views edit, still inside its debounce: tick the first column off.
    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    assert!(!edit_draft(&shell, &cx, |d| d
        .list_items("columns")
        .unwrap()[0]
        .included));
    std::fs::write(dir.path().join("colors.toml"), "[delta\nhue =").unwrap();
    open_palette_action(&mut cx, "Edit colors");
    cx.simulate_keystrokes("enter space");
    cx.run_until_parked();
    flush_config_write(&mut cx);
    assert!(
        dialog_state(&shell, &cx, |s| s.notice.clone())
            .is_some_and(|n| n.contains("could not save")),
        "Colors, the second contributor, says its write failed"
    );

    cx.simulate_keystrokes("escape escape");
    cx.run_until_parked();
    assert_eq!(domains(&shell, &cx), vec![Domain::Views]);
    assert!(
        dialog_state(&shell, &cx, |s| s.notice.clone())
            .is_some_and(|n| n.contains("could not save")),
        "Views' edit rode the failed batch, so Views says so too"
    );
    assert!(
        edit_draft(&shell, &cx, |d| d.list_items("columns").unwrap()[0]
            .included),
        "and paints the reverted value"
    );
}

/// A Scopes Values stage covered by Colors still receives its values reply, and
/// shows it once revealed.
#[gpui::test]
fn a_covered_scopes_values_stage_receives_its_delivery(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    cx.simulate_keystrokes("enter"); // mine
    cx.simulate_keystrokes("enter"); // book's Values stage
    cx.run_until_parked();
    let tag = dialog_state(&shell, &cx, |s| s.values_tag);
    dispatch_action(&shell, "config::colors", &mut cx);
    assert_eq!(domains(&shell, &cx), vec![Domain::Scopes, Domain::Colors]);

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
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(domains(&shell, &cx), vec![Domain::Scopes]);
    let names: Vec<String> = edit_draft(&shell, &cx, |d| {
        d.list_items("values")
            .unwrap()
            .iter()
            .map(|i| i.name.clone())
            .collect()
    });
    assert_eq!(names, ["BK000", "BK001"]);
}

/// A covered Scopes expression field receives its `EXPR_KEY` reply.
#[gpui::test]
fn a_covered_expression_field_receives_its_values(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    cx.update(|_, app| {
        let seen = seen.clone();
        app.subscribe(&shell, move |_, e: &ShellEvent, _| {
            if let ShellEvent::DistinctRequested(p) = e {
                seen.borrow_mut().push(p.clone());
            }
        })
        .detach();
    });
    open_expression_field(&shell, &mut cx);
    cx.simulate_input("book = ");
    cx.run_until_parked();
    let req = seen.borrow().last().cloned().expect("a request");
    dispatch_action(&shell, "config::colors", &mut cx);
    assert_eq!(domains(&shell, &cx), vec![Domain::Scopes, Domain::Colors]);

    let holds_value = |shell: &Entity<ShellView>, cx: &gpui::VisualTestContext| {
        shell.read_with(cx, |s, _| {
            s.modals[0]
                .parked_object
                .as_ref()
                .and_then(|p| p.state.expr.as_ref())
                .is_some_and(|c| c.rows().iter().any(|r| format!("{r:?}").contains("BK777")))
        })
    };
    assert!(!holds_value(&shell, &cx));
    shell.update(&mut cx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: EXPR_KEY,
                tag: req.tag,
                column: req.column.clone(),
                values: Ok(vec![("BK777".into(), 7)]),
            },
            cx,
        )
    });
    assert!(
        holds_value(&shell, &cx),
        "the covered field's completion holds the delivered value"
    );
}

/// A reload that defines a new column re-ranks a covered Scopes expression field at
/// once: its unknown-column warning is gone while it is still covered.
#[gpui::test]
fn a_reload_refreshes_a_covered_object_expression_field(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    open_expression_field(&shell, &mut cx);
    cx.simulate_input("zzcol = 'x'");
    cx.run_until_parked();
    dispatch_action(&shell, "config::colors", &mut cx);
    let warning = |shell: &Entity<ShellView>, cx: &gpui::VisualTestContext| {
        shell.read_with(cx, |s, _| {
            s.modals[0]
                .parked_object
                .as_ref()
                .and_then(|p| p.state.expr.as_ref())
                .map(|c| c.warning().is_some())
        })
    };
    assert_eq!(
        warning(&shell, &cx),
        Some(true),
        "zzcol is not yet a column"
    );

    shell.update(&mut cx, |s, cx| {
        let mut docs = s.services.config.all_docs();
        docs.push(LayerDoc {
            layer: Layer::User,
            name: "datasets".to_string(),
            file: "<test:user>".into(),
            table: "[risk.columns.zzcol]\ntype = \"utf8\"\nrole = \"attribute\"\n\
                    grain = \"position\"\n"
                .parse()
                .unwrap(),
        });
        s.apply_reload(Config::from_docs(docs), cx)
    });
    assert_eq!(
        warning(&shell, &cx),
        Some(false),
        "the covered field re-ranks at reload, not at its next keystroke"
    );
}
