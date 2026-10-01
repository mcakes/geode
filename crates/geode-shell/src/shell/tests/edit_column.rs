//! `config::view_column` / `config::schema_column` through the palette: the
//! focused tile's columns in a list, the cursor's preselected, a pick landing
//! on that column's Column stage.

use super::objectdialog::{desk_view_services, dialog_state};
use super::*;
use crate::defaults::AddPlacement;
use crate::module::recording::RecordingFactory;
use crate::shell::choicedialog::{NO_TILE_COLUMNS, Target};
use crate::shell::dialog::DialogKind;
use crate::shell::objectdialog::{Domain, Stage};
use geode_core::tile_columns::{TileColumn, TileColumns};

/// Open the palette, type `title`, press Enter: the trader's route.
fn open_palette_action(cx: &mut gpui::VisualTestContext, title: &str) {
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input(title);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
}

/// The desk view fixture (`tree` over `risk_snapshot`: `book`, `npv` labelled
/// `NPV`) with a focused "rec" tile answering `columns`.
fn shell_with_tile(
    cx: &mut gpui::TestAppContext,
    columns: Option<TileColumns>,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    let rec = RecordingFactory::new("rec");
    *rec.tile_columns.borrow_mut() = columns;
    let mut services = services_with_recorders(vec![rec]);
    let desk = desk_view_services(&[]);
    (services.config, services.builtin) = (desk.config, desk.builtin);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.add_tile("rec", AddPlacement::Split(None), None, window, cx);
        })
    });
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.run_until_parked();
    (shell, vcx)
}

fn tree(active: Option<usize>) -> TileColumns {
    TileColumns {
        view: "tree".into(),
        columns: vec![
            TileColumn {
                name: "book".into(),
                label: "book".into(),
                derived: false,
            },
            TileColumn {
                name: "npv".into(),
                label: "NPV".into(),
                derived: false,
            },
        ],
        active,
    }
}

fn stage(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> (Domain, Stage) {
    dialog_state(shell, cx, |s| (s.domain, s.stage.clone()))
}

#[gpui::test]
fn enter_opens_views_on_the_cursor_column(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = shell_with_tile(cx, Some(tree(Some(1))));
    open_palette_action(&mut cx, "Edit column in view");
    assert!(
        shell.read_with(&cx, |s, _| matches!(
            s.choice_dialog.as_ref().map(|d| &d.target),
            Some(Target::Column {
                domain: Domain::Views,
                ..
            })
        )),
        "the column list is open"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        stage(&shell, &cx),
        (
            Domain::Views,
            Stage::Column {
                object: "tree".into(),
                column: "npv".into()
            }
        )
    );
    assert!(
        shell.read_with(&cx, |s, _| s.choice_dialog.is_none()),
        "the list closed"
    );
}

#[gpui::test]
fn typing_picks_a_different_column(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = shell_with_tile(cx, Some(tree(Some(1))));
    open_palette_action(&mut cx, "Edit column in view");
    cx.simulate_input("book");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        stage(&shell, &cx),
        (
            Domain::Views,
            Stage::Column {
                object: "tree".into(),
                column: "book".into()
            }
        )
    );
}

#[gpui::test]
fn schema_opens_the_owning_dataset_on_the_column(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = shell_with_tile(cx, Some(tree(Some(1))));
    open_palette_action(&mut cx, "Edit column in schema");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        stage(&shell, &cx),
        (
            Domain::Schema,
            Stage::Column {
                object: "risk_snapshot".into(),
                column: "npv".into()
            }
        )
    );
}

#[gpui::test]
fn escape_steps_back_to_the_view_then_browse(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = shell_with_tile(cx, Some(tree(Some(1))));
    open_palette_action(&mut cx, "Edit column in view");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        stage(&shell, &cx),
        (
            Domain::Views,
            Stage::Edit {
                object: "tree".into()
            }
        )
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(stage(&shell, &cx), (Domain::Views, Stage::Browse));
}

#[gpui::test]
fn a_tile_without_columns_gets_a_notice_and_no_list(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = shell_with_tile(cx, None);
    open_palette_action(&mut cx, "Edit column in view");
    shell.read_with(&cx, |s, _| {
        assert!(s.choice_dialog.is_none());
        assert!(s.object_dialog.is_none());
        assert_eq!(s.notice.as_deref(), Some(NO_TILE_COLUMNS));
    });
}

#[gpui::test]
fn a_covered_views_dialog_refuses_before_the_list_opens(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = shell_with_tile(cx, Some(tree(Some(1))));
    open_palette_action(&mut cx, "Edit views");
    open_palette_action(&mut cx, "Edit colors");
    open_palette_action(&mut cx, "Edit column in view");
    shell.read_with(&cx, |s, _| {
        assert!(
            s.choice_dialog.is_none(),
            "no list for a dialog that cannot open"
        );
        assert_eq!(
            s.notice.as_deref(),
            Some(Domain::Views.already_open_notice())
        );
        assert_eq!(s.top_kind(), Some(DialogKind::Object));
    });
}

#[gpui::test]
fn an_undefined_view_lands_in_browse_with_a_notice(cx: &mut gpui::TestAppContext) {
    let mut gone = tree(Some(1));
    gone.view = "gone".into();
    let (shell, mut cx) = shell_with_tile(cx, Some(gone));
    open_palette_action(&mut cx, "Edit column in view");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    dialog_state(&shell, &cx, |s| {
        assert_eq!(s.domain, Domain::Views);
        assert_eq!(s.stage, Stage::Browse);
        assert_eq!(s.notice.as_deref(), Some("view 'gone' is not defined"));
    });
}

#[gpui::test]
fn a_column_the_view_lacks_stops_at_its_edit_stage(cx: &mut gpui::TestAppContext) {
    let mut stale = tree(Some(0));
    stale.columns[0] = TileColumn {
        name: "vanished".into(),
        label: "vanished".into(),
        derived: false,
    };
    let (shell, mut cx) = shell_with_tile(cx, Some(stale));
    open_palette_action(&mut cx, "Edit column in view");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    dialog_state(&shell, &cx, |s| {
        assert_eq!(
            s.stage,
            Stage::Edit {
                object: "tree".into()
            }
        );
        assert_eq!(
            s.notice.as_deref(),
            Some("'vanished' is not a column of 'tree'")
        );
    });
}

/// A column no dataset of the view declares (a derived dimension the view lists as
/// an ordinary `dimension` column, so the tile cannot flag it) is not offered by the
/// Schema list: a pick could only fail.
#[gpui::test]
fn schema_omits_a_column_no_dataset_declares(cx: &mut gpui::TestAppContext) {
    let mut t = tree(Some(1));
    t.columns.push(TileColumn {
        name: "desk".into(),
        label: "desk".into(),
        derived: false,
    });
    let (shell, mut cx) = shell_with_tile(cx, Some(t));
    open_palette_action(&mut cx, "Edit column in schema");
    let options = shell.read_with(&cx, |s, _| {
        s.choice_dialog
            .as_ref()
            .map(|d| d.list.options().to_vec())
            .unwrap_or_default()
    });
    assert_eq!(options, ["book", "NPV · npv"]);
}

#[gpui::test]
fn enter_with_every_row_filtered_out_commits_nothing(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = shell_with_tile(cx, Some(tree(Some(1))));
    open_palette_action(&mut cx, "Edit column in view");
    cx.simulate_input("zzzz");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    shell.read_with(&cx, |s, _| {
        assert!(s.choice_dialog.is_some(), "the list stays open");
        assert!(s.object_dialog.is_none());
    });
}
