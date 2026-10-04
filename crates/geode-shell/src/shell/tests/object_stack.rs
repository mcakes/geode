//! Object dialogs of different domains stack over each other. The covered one is
//! parked in its own stack entry and comes back exactly as it was left; the same
//! domain never nests.

use super::objectdialog::{desk_view_services, dialog_state, edit_draft, flush_config_write};
use super::*;
use crate::shell::dialog::DialogKind;
use crate::shell::objectdialog::{self, Domain};

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
        shell.read_with(&cx, |s, _| s.notice.clone()).as_deref(),
        Some("views is already open underneath")
    );
    // Asking for the domain already on top does nothing and says nothing.
    shell.update(&mut cx, |s, _| s.notice = None);
    dispatch_action(&shell, "config::colors", &mut cx);
    assert_eq!(domains(&shell, &cx), vec![Domain::Views, Domain::Colors]);
    assert_eq!(shell.read_with(&cx, |s, _| s.notice.clone()), None);
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
    open_palette_action(&mut cx, "Edit sources");
    assert_eq!(
        domains(&shell, &cx),
        vec![Domain::Views, Domain::Colors, Domain::Sources]
    );
    dispatch_action(&shell, "config::colors", &mut cx);
    assert_eq!(
        domains(&shell, &cx),
        vec![Domain::Views, Domain::Colors, Domain::Sources]
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s.notice.clone()).as_deref(),
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

/// The flow the feature exists for: a column needs a color that does not exist yet.
/// Create it in a stacked Colors dialog, come back, and it is among the column's
/// color choices, with the column stage exactly where it was and nothing dirtied.
#[gpui::test]
fn a_color_created_in_a_stacked_dialog_is_offered_to_the_covered_column(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage_with(cx, views_with_colors(), dir.path());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let stage = dialog_state(&shell, &cx, |s| s.stage.clone());
    let color_options = |shell: &Entity<ShellView>, cx: &gpui::VisualTestContext| {
        edit_draft(shell, cx, |d| {
            d.fields
                .iter()
                .find(|f| f.key == "color")
                .and_then(|f| match &f.kind {
                    objectdialog::FieldKind::Choice { options, .. } => Some(options.clone()),
                    _ => None,
                })
                .unwrap()
        })
    };
    assert!(!color_options(&shell, &cx).contains(&"ember".to_string()));

    open_palette_action(&mut cx, "Edit colors");
    cx.simulate_keystrokes("n");
    cx.simulate_input("ember");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    flush_config_write(&mut cx);
    // Escape out of Colors, however many stages it takes, and no further.
    for _ in 0..3 {
        if domains(&shell, &cx) == vec![Domain::Views] {
            break;
        }
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
    }
    assert_eq!(domains(&shell, &cx), vec![Domain::Views]);
    assert_eq!(dialog_state(&shell, &cx, |s| s.stage.clone()), stage);
    assert!(
        color_options(&shell, &cx).contains(&"ember".to_string()),
        "{:?}",
        color_options(&shell, &cx)
    );
    assert!(
        !edit_draft(&shell, &cx, |d| d.is_dirty()),
        "and the column is not dirtied"
    );
}

fn input_state(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> (String, usize) {
    shell.read_with(cx, |s, cx| {
        let input = s.dialog_input.read(cx);
        (input.value().to_string(), input.cursor())
    })
}

/// An open value field in the covered Views dialog, with typed text and the caret
/// mid-text, survives a Colors push and pop: the field is still open, the text and
/// caret come back, and typing continues into it.
#[gpui::test]
fn an_open_value_field_survives_a_stacked_colors_dialog(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage_with(cx, views_with_colors(), dir.path());
    cx.simulate_keystrokes("enter"); // column stage, cursor on Label
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.selected_row().is_some_and(
        |r| matches!(r, objectdialog::EditRow::Field(i) if d.fields[i].key == "label")
    )));
    cx.simulate_keystrokes("i");
    cx.simulate_input("abc");
    cx.simulate_keystrokes("left");
    cx.run_until_parked();
    assert_eq!(input_state(&shell, &cx), ("abc".to_string(), 2));

    open_palette_action(&mut cx, "Edit colors");
    assert_eq!(domains(&shell, &cx), vec![Domain::Views, Domain::Colors]);
    assert_eq!(
        input_state(&shell, &cx).0,
        "",
        "Colors starts on a clear input"
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(domains(&shell, &cx), vec![Domain::Views]);
    assert!(
        edit_draft(&shell, &cx, |d| d.text_entry.is_some()),
        "the field is still open"
    );
    assert_eq!(input_state(&shell, &cx), ("abc".to_string(), 2));
    cx.simulate_input("X");
    cx.run_until_parked();
    assert_eq!(edit_draft(&shell, &cx, |d| d.query.clone()), "abXc");
}

/// A color typeahead open on the covered column's color row: after a color is created
/// in the stacked Colors dialog, the revealed typeahead lists it and keeps its query.
#[gpui::test]
fn an_open_color_typeahead_lists_a_color_created_above_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage_with(cx, views_with_colors(), dir.path());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    for _ in 0..8 {
        let on_color = edit_draft(&shell, &cx, |d| {
            d.selected_row().is_some_and(
                |r| matches!(r, objectdialog::EditRow::Field(i) if d.fields[i].key == "color"),
            )
        });
        if on_color {
            break;
        }
        cx.simulate_keystrokes("j");
    }
    cx.simulate_keystrokes("i");
    cx.simulate_input("emb");
    cx.run_until_parked();
    assert!(
        edit_draft(&shell, &cx, |d| d.choice_entry()),
        "the color typeahead is open"
    );

    open_palette_action(&mut cx, "Edit colors");
    assert_eq!(domains(&shell, &cx), vec![Domain::Views, Domain::Colors]);
    cx.simulate_keystrokes("n");
    cx.simulate_input("ember");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    flush_config_write(&mut cx);
    for _ in 0..3 {
        if domains(&shell, &cx) == vec![Domain::Views] {
            break;
        }
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
    }
    assert_eq!(domains(&shell, &cx), vec![Domain::Views]);
    let (options, query, lit) = edit_draft(&shell, &cx, |d| {
        let list = d.choice.as_ref().expect("the typeahead is still open");
        (
            list.options().to_vec(),
            list.query().to_string(),
            list.highlighted_text().map(str::to_string),
        )
    });
    assert!(options.contains(&"ember".to_string()), "{options:?}");
    assert_eq!(query, "emb");
    assert_eq!(lit.as_deref(), Some("ember"));
    assert_eq!(input_state(&shell, &cx).0, "emb");
}

/// A filtered views list parked under a Colors dialog; a reload adds a view
/// while it is covered. Popping the cover must paint the new list, still filtered.
#[gpui::test]
fn a_reload_under_a_covering_object_dialog_repaints_the_revealed_list(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, views_with_colors(), dir.path(), "config::views");
    cx.simulate_keystrokes("/");
    cx.simulate_input("r");
    cx.simulate_keystrokes("enter");
    dispatch_action(&shell, "config::colors", &mut cx);
    assert_eq!(domains(&shell, &cx), vec![Domain::Views, Domain::Colors]);
    std::fs::write(
        dir.path().join("views.toml"),
        "config_version = 1\n[wider]\ndataset = \"risk_snapshot\"\n[[wider.columns]]\nname = \"npv\"\n",
    )
    .unwrap();
    let builtin = shell.read_with(&cx, |s, _| s.services.builtin.clone());
    let config = crate::reload::load_config(builtin, None, Some(dir.path().to_path_buf()));
    shell.update(&mut cx, |s, cx| s.apply_reload(config, cx));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| (s.domain, s.query.clone())),
        (Domain::Views, "r".to_string())
    );
    assert!(super::objectdialog::fresh_browse_names(&shell, &cx).contains(&"wider".to_string()));
    super::objectdialog::assert_browse_rows_are_fresh(&shell, &mut cx);
}
