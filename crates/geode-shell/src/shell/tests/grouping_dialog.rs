//! The Grouping dialog: one list that applies a row, edits a row, types an
//! ad hoc chain and saves a chain to a slot. The fixture's default modifier
//! is Alt, so `mod+g` is `alt-g` and `mod+s` is `alt-s`.

use super::objectdialog::dialog_state;
use super::*;
use crate::shell::objectdialog;

/// The action that opens the dialog. One place, so the door can change.
pub(super) const DOOR: &str = "config::groupings";

/// `book`, `lhu` and `position_ref` are groupable (the position-grain
/// measure declares the grain that carries them). Slot 1 is `book / lhu`
/// and slot 3 is `lhu`, both builtin; the rest are empty.
pub(super) fn services() -> ShellServices {
    let mut services = test_services();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
         [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
    )
    .unwrap();
    let groupings =
        LayerDoc::builtin("groupings", "1 = [\"book\", \"lhu\"]\n3 = [\"lhu\"]\n").unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            datasets,
            groupings,
        ],
        ..ConfigSources::default()
    });
    services
}

pub(super) fn chain(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| n.to_string()).collect()
}

pub(super) fn frame_of(
    shell: &Entity<ShellView>,
    cx: &gpui::VisualTestContext,
) -> Entity<crate::frame::Frame> {
    shell.read_with(cx, |s, _| s.frame().clone())
}

/// Open the dialog on a fresh shell with a writable user directory.
pub(super) fn open_dialog(
    cx: &mut gpui::TestAppContext,
) -> (
    Entity<ShellView>,
    gpui::VisualTestContext,
    tempfile::TempDir,
) {
    open_dialog_after(cx, |_| {})
}

/// Prepare the frame, then open the dialog: for a test whose subject is how
/// the dialog opens on an existing choice.
pub(super) fn open_dialog_after(
    cx: &mut gpui::TestAppContext,
    prepare: impl FnOnce(&mut crate::frame::Frame),
) -> (
    Entity<ShellView>,
    gpui::VisualTestContext,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let (window, mut vcx) = open_shell_with_user_dir(cx, services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    let frame = frame_of(&shell, &vcx);
    frame.update(&mut vcx, |f, cx| {
        prepare(f);
        cx.notify();
    });
    vcx.run_until_parked();
    dispatch_action(&shell, DOOR, &mut vcx);
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    (shell, vcx, dir)
}

pub(super) fn row_names(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> Vec<String> {
    dialog_state(shell, cx, |s| {
        s.rows.rows().iter().map(|r| r.name.clone()).collect()
    })
}

/// The name of the row under the list cursor.
pub(super) fn cursor_name(
    shell: &Entity<ShellView>,
    cx: &gpui::VisualTestContext,
) -> Option<String> {
    dialog_state(shell, cx, |s| s.rows.at(s.selected).map(|r| r.name.clone()))
}

fn summary_of(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext, name: &str) -> String {
    dialog_state(shell, cx, |s| {
        s.rows
            .rows()
            .iter()
            .find(|r| r.name == name)
            .map(|r| r.summary.clone())
            .unwrap_or_default()
    })
}

#[gpui::test]
fn the_list_leads_with_the_view_default_and_the_ad_hoc_row(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog(cx);
    assert_eq!(
        row_names(&shell, &cx),
        ["0", "*", "1", "2", "3", "4", "5", "6", "7", "8", "9"]
    );
    assert_eq!(summary_of(&shell, &cx, "0"), "view default");
    assert_eq!(summary_of(&shell, &cx, "*"), "no ad hoc chain");
    assert_eq!(summary_of(&shell, &cx, "1"), "book / lhu");
    assert!(cx.debug_bounds("objectdialog-row-0").is_some());
    assert!(cx.debug_bounds("objectdialog-row-*").is_some());
    let last = cx
        .debug_bounds("objectdialog-row-9")
        .expect("slot 9's row paints");
    let list = cx
        .debug_bounds("objectdialog-list")
        .expect("the list paints");
    assert!(
        last.bottom() <= list.bottom(),
        "all eleven rows fit without scrolling: row 9 ends at {:?}, the list at {:?}",
        last.bottom(),
        list.bottom()
    );
    let crumb = shell.read_with(&cx, |shell, _| objectdialog::render::crumb_text(shell));
    assert_eq!(crumb, "9 slots", "the two leading rows are not slots");
    assert_eq!(
        cursor_name(&shell, &cx).as_deref(),
        Some("0"),
        "nothing is active, so the dialog opens on the view default"
    );
    assert!(cx.debug_bounds("objectdialog-active").is_some());
}

#[gpui::test]
fn the_dialog_opens_on_the_active_slot(cx: &mut gpui::TestAppContext) {
    let (shell, cx, _dir) = open_dialog_after(cx, |f| {
        assert!(f.shared_mut().set_active_slot(Some(3)));
    });
    assert_eq!(cursor_name(&shell, &cx).as_deref(), Some("3"));
}

#[gpui::test]
fn the_dialog_opens_on_an_active_ad_hoc_chain_and_tags_it(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog_after(cx, |f| {
        assert!(f.shared_mut().set_ad_hoc(chain(&["lhu", "book"])));
    });
    assert_eq!(cursor_name(&shell, &cx).as_deref(), Some("*"));
    assert_eq!(summary_of(&shell, &cx, "*"), "lhu / book");
    assert!(cx.debug_bounds("objectdialog-adhoc").is_some());
}

#[gpui::test]
fn the_ad_hoc_row_follows_the_frame_while_the_dialog_is_open(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog_after(cx, |f| {
        f.shared_mut().set_ad_hoc(chain(&["lhu"]));
        f.shared_mut().set_active_slot(Some(1));
    });
    assert_eq!(summary_of(&shell, &cx, "*"), "lhu");
    let frame = frame_of(&shell, &cx);
    frame.update(&mut cx, |f, cx| {
        assert!(f.shared_mut().forget_ad_hoc());
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        summary_of(&shell, &cx, "*"),
        "no ad hoc chain",
        "a stale row would offer a chain that no longer exists"
    );
}

#[gpui::test]
fn the_dialog_is_titled_grouping(cx: &mut gpui::TestAppContext) {
    let (shell, cx, _dir) = open_dialog(cx);
    let title = shell.read_with(&cx, |s, _| s.modals.last().map(|m| m.title.to_string()));
    assert_eq!(title.as_deref(), Some("Grouping"));
}
