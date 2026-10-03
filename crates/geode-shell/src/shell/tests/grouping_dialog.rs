//! The Grouping dialog: one list that applies a row, edits a row, types an
//! ad hoc chain and saves a chain to a slot. The fixture's default modifier
//! is Alt, so `mod+g` is `alt-g` and `mod+s` is `alt-s`.

use super::objectdialog::{dialog_state, edit_draft, flush_config_write};
use super::*;
use crate::frame::GroupingChoice;
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

fn written(dir: &tempfile::TempDir) -> String {
    std::fs::read_to_string(dir.path().join("groupings.toml")).unwrap_or_default()
}

fn choice(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> GroupingChoice {
    frame_of(shell, cx).read_with(cx, |f, _| f.shared().grouping_choice())
}

fn in_force(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> Option<Vec<String>> {
    frame_of(shell, cx).read_with(cx, |f, _| {
        f.shared().active_grouping().map(<[String]>::to_vec)
    })
}

fn is_open(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> bool {
    shell.read_with(cx, |s, _| s.modal_open())
}

fn notice(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> String {
    dialog_state(shell, cx, |s| s.notice.clone()).unwrap_or_default()
}

#[gpui::test]
fn a_digit_applies_a_filled_slot_and_closes(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog(cx);
    cx.simulate_keystrokes("3");
    cx.run_until_parked();
    assert_eq!(choice(&shell, &cx), GroupingChoice::Slot(3));
    assert!(!is_open(&shell, &cx));
}

#[gpui::test]
fn enter_applies_the_cursor_row_and_closes(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog(cx);
    cx.simulate_keystrokes("j j enter"); // row 2 is slot 1
    cx.run_until_parked();
    assert_eq!(choice(&shell, &cx), GroupingChoice::Slot(1));
    assert!(!is_open(&shell, &cx));
}

#[gpui::test]
fn zero_returns_to_the_view_default_from_any_row(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog_after(cx, |f| {
        f.shared_mut().set_active_slot(Some(3));
    });
    cx.simulate_keystrokes("0");
    cx.run_until_parked();
    assert_eq!(choice(&shell, &cx), GroupingChoice::ViewDefault);
    assert!(!is_open(&shell, &cx));
}

#[gpui::test]
fn enter_on_the_choice_already_in_force_closes_without_a_requery(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog_after(cx, |f| {
        f.shared_mut().set_active_slot(Some(3));
    });
    let before = frame_of(&shell, &cx).read_with(&cx, |f, _| f.shared().versions());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(!is_open(&shell, &cx));
    assert_eq!(
        frame_of(&shell, &cx).read_with(&cx, |f, _| f.shared().versions()),
        before
    );
}

#[gpui::test]
fn a_row_click_applies_the_slot(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog(cx);
    let row = cx
        .debug_bounds("objectdialog-row-3")
        .expect("slot 3 paints");
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(40.0), row.origin.y + gpui::px(4.0)),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert_eq!(choice(&shell, &cx), GroupingChoice::Slot(3));
    assert!(!is_open(&shell, &cx));
}

#[gpui::test]
fn e_opens_the_tick_list_editor_and_escape_returns_to_the_list(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog(cx);
    cx.simulate_keystrokes("j j j j e"); // row 4 is slot 3
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "3".to_string()
        }
    );
    assert!(
        !edit_draft(&shell, &cx, |d| d.chain_entry()),
        "the tick list, not the field"
    );
    assert_eq!(
        choice(&shell, &cx),
        GroupingChoice::ViewDefault,
        "editing applies nothing"
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    );
    assert_eq!(cursor_name(&shell, &cx).as_deref(), Some("3"));
}

#[gpui::test]
fn e_opens_the_tick_list_for_an_empty_slot_too(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog(cx);
    cx.simulate_keystrokes("j j j e"); // row 3 is slot 2, empty
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "2".to_string()
        }
    );
    assert!(!edit_draft(&shell, &cx, |d| d.chain_entry()));
}

#[gpui::test]
fn e_on_the_view_default_is_refused_with_a_notice(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog(cx);
    cx.simulate_keystrokes("e");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    );
    assert_eq!(
        notice(&shell, &cx),
        objectdialog::grouping_list::NOTHING_TO_EDIT
    );
}

#[gpui::test]
fn a_digit_on_an_empty_slot_defines_it_activates_it_and_closes(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, dir) = open_dialog(cx);
    cx.simulate_keystrokes("6");
    cx.run_until_parked();
    assert!(is_open(&shell, &cx), "an empty slot has nothing to apply");
    assert!(
        edit_draft(&shell, &cx, |d| d.chain_entry()),
        "its chain field is open"
    );
    assert_eq!(choice(&shell, &cx), GroupingChoice::ViewDefault);

    cx.simulate_input("lhu book");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert!(!is_open(&shell, &cx));
    assert_eq!(
        choice(&shell, &cx),
        GroupingChoice::Slot(6),
        "activated on the keystroke that defined it, ahead of the debounced write"
    );
    assert_eq!(in_force(&shell, &cx), Some(chain(&["lhu", "book"])));

    flush_config_write(&mut cx);
    assert!(
        written(&dir).contains("6 = [\"lhu\", \"book\"]"),
        "{}",
        written(&dir)
    );
    assert_eq!(
        choice(&shell, &cx),
        GroupingChoice::Slot(6),
        "the write's reload keeps it"
    );
}

#[gpui::test]
fn escape_from_a_list_opened_chain_field_returns_to_the_list(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, dir) = open_dialog(cx);
    cx.simulate_keystrokes("6");
    cx.run_until_parked();
    cx.simulate_input("lhu");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    );
    assert_eq!(cursor_name(&shell, &cx).as_deref(), Some("6"));
    flush_config_write(&mut cx);
    assert_eq!(written(&dir), "", "nothing was written");
    assert_eq!(choice(&shell, &cx), GroupingChoice::ViewDefault);
}

#[gpui::test]
fn a_refused_chain_keeps_the_list_opened_field_open(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog(cx);
    cx.simulate_keystrokes("6");
    cx.run_until_parked();
    cx.simulate_input("lhu nope");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(is_open(&shell, &cx));
    assert!(
        edit_draft(&shell, &cx, |d| d.chain_entry()),
        "a typo is fixed, not retyped"
    );
    assert!(
        notice(&shell, &cx).contains("nope"),
        "{}",
        notice(&shell, &cx)
    );
    assert_eq!(choice(&shell, &cx), GroupingChoice::ViewDefault);
}

#[gpui::test]
fn a_failed_slot_write_takes_the_staged_slot_back_out(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, dir) = open_dialog(cx);
    // Unparseable and never loaded by this shell: memory accepts the edit,
    // and only the write discovers the problem.
    std::fs::write(dir.path().join("groupings.toml"), "6 = [\n").unwrap();
    cx.simulate_keystrokes("6");
    cx.run_until_parked();
    cx.simulate_input("lhu");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(choice(&shell, &cx), GroupingChoice::Slot(6));

    flush_config_write(&mut cx);

    assert_eq!(
        choice(&shell, &cx),
        GroupingChoice::ViewDefault,
        "a chain persisted nowhere must not stay in force"
    );
    assert_eq!(
        frame_of(&shell, &cx).read_with(&cx, |f, _| f.slots().get(6).map(<[String]>::to_vec)),
        None
    );
}

#[gpui::test]
fn enter_in_filter_mode_keeps_the_filter_and_applies_nothing(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog(cx);
    cx.simulate_keystrokes("/");
    cx.run_until_parked();
    cx.simulate_input("book");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        is_open(&shell, &cx),
        "filter-mode enter keeps the query, nothing more"
    );
    assert_eq!(choice(&shell, &cx), GroupingChoice::ViewDefault);
    assert_eq!(dialog_state(&shell, &cx, |s| s.query.clone()), "book");
    assert_eq!(
        cursor_name(&shell, &cx).as_deref(),
        Some("1"),
        "book / lhu is the match"
    );

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(choice(&shell, &cx), GroupingChoice::Slot(1));
    assert!(!is_open(&shell, &cx));
}

#[gpui::test]
fn a_digit_typed_into_the_filter_is_text_not_a_pick(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog(cx);
    cx.simulate_keystrokes("/");
    cx.run_until_parked();
    cx.simulate_input("3");
    cx.run_until_parked();
    assert!(is_open(&shell, &cx));
    assert_eq!(choice(&shell, &cx), GroupingChoice::ViewDefault);
    assert_eq!(dialog_state(&shell, &cx, |s| s.query.clone()), "3");
}

/// The first click on an empty slot opens its chain field, whose completion
/// rows paint where the list was: slot 2's row lies under the `book`
/// completion. The double-click's second half must not complete it.
#[gpui::test]
fn a_double_click_on_an_empty_slot_opens_its_field_and_inserts_nothing(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, mut cx, _dir) = open_dialog(cx);
    let row = cx
        .debug_bounds("objectdialog-row-2")
        .expect("slot 2 paints");
    let at = gpui::point(row.origin.x + gpui::px(40.0), row.origin.y + gpui::px(4.0));
    double_click(&mut cx, at, gpui::Modifiers::none());
    cx.run_until_parked();

    let book = cx
        .debug_bounds("objectdialog-item-book")
        .expect("the field offers book");
    assert!(
        book.contains(&at),
        "the second click landed on a completion row: {book:?} vs {at:?}"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "2".to_string()
        }
    );
    assert!(
        edit_draft(&shell, &cx, |d| d.chain_entry()),
        "the field is open"
    );
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.query.clone()),
        "",
        "the second click completed nothing into the field"
    );
    assert_eq!(choice(&shell, &cx), GroupingChoice::ViewDefault);
}

/// A click opens an empty slot's field as a digit does, and the field owns
/// the keys typed after it.
#[gpui::test]
fn a_click_on_an_empty_slot_then_typing_defines_and_activates_it(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, dir) = open_dialog(cx);
    let row = cx
        .debug_bounds("objectdialog-row-6")
        .expect("slot 6 paints");
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(40.0), row.origin.y + gpui::px(4.0)),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.chain_entry()));

    cx.simulate_input("lhu book");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert!(!is_open(&shell, &cx));
    assert_eq!(choice(&shell, &cx), GroupingChoice::Slot(6));
    assert_eq!(in_force(&shell, &cx), Some(chain(&["lhu", "book"])));
    flush_config_write(&mut cx);
    assert!(
        written(&dir).contains("6 = [\"lhu\", \"book\"]"),
        "{}",
        written(&dir)
    );
}
