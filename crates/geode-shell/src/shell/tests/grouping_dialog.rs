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

fn stored(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> Option<Vec<String>> {
    frame_of(shell, cx).read_with(cx, |f, _| f.shared().ad_hoc().map(<[String]>::to_vec))
}

#[gpui::test]
fn i_types_an_ad_hoc_chain_and_enter_applies_it_with_no_file_write(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, dir) = open_dialog(cx);
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "*".to_string()
        }
    );
    assert!(edit_draft(&shell, &cx, |d| d.chain_entry()));
    let crumb = shell.read_with(&cx, |shell, _| objectdialog::render::crumb_text(shell));
    assert_eq!(crumb, "ad hoc");

    cx.simulate_input("lhu book");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert!(!is_open(&shell, &cx));
    assert_eq!(choice(&shell, &cx), GroupingChoice::AdHoc);
    assert_eq!(in_force(&shell, &cx), Some(chain(&["lhu", "book"])));
    flush_config_write(&mut cx);
    assert_eq!(
        written(&dir),
        "",
        "the ad hoc chain is never written to groupings.toml"
    );
}

#[gpui::test]
fn i_is_seeded_from_the_cursor_row_and_enter_applies_it_unchanged(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog(cx);
    cx.simulate_keystrokes("j j i"); // row 2 is slot 1: book / lhu
    cx.run_until_parked();
    let field = shell.read_with(&cx, |s, cx| s.dialog_input.read(cx).value().to_string());
    assert_eq!(field, "book / lhu");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        choice(&shell, &cx),
        GroupingChoice::AdHoc,
        "an untouched seed is still a request to apply that chain ad hoc"
    );
    assert_eq!(in_force(&shell, &cx), Some(chain(&["book", "lhu"])));
    assert_eq!(
        frame_of(&shell, &cx).read_with(&cx, |f, _| f.slots().get(1).map(<[String]>::to_vec)),
        Some(chain(&["book", "lhu"])),
        "the slot it was seeded from is untouched"
    );
}

#[gpui::test]
fn escape_from_the_ad_hoc_field_returns_to_the_list_and_changes_nothing(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, mut cx, _dir) = open_dialog(cx);
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    cx.simulate_input("lhu");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    );
    assert_eq!(stored(&shell, &cx), None);
    assert_eq!(cursor_name(&shell, &cx).as_deref(), Some("*"));
}

#[gpui::test]
fn enter_and_a_on_an_empty_ad_hoc_row_open_its_chain_field(cx: &mut gpui::TestAppContext) {
    for keys in ["j enter", "a"] {
        let (shell, mut cx, _dir) = open_dialog(cx);
        cx.simulate_keystrokes(keys);
        cx.run_until_parked();
        assert!(is_open(&shell, &cx), "{keys}");
        assert!(edit_draft(&shell, &cx, |d| d.chain_entry()), "{keys}");
    }
}

#[gpui::test]
fn a_returns_to_a_stored_ad_hoc_chain(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog_after(cx, |f| {
        f.shared_mut().set_ad_hoc(chain(&["lhu"]));
        f.shared_mut().set_active_slot(Some(1));
    });
    cx.simulate_keystrokes("a");
    cx.run_until_parked();
    assert_eq!(choice(&shell, &cx), GroupingChoice::AdHoc);
    assert!(!is_open(&shell, &cx));
}

#[gpui::test]
fn ticking_in_the_ad_hoc_editor_regroups_at_once(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, dir) = open_dialog_after(cx, |f| {
        f.shared_mut().set_ad_hoc(chain(&["lhu"]));
        f.shared_mut().set_active_slot(Some(1));
    });
    cx.simulate_keystrokes("k k"); // from slot 1 (row 2) up to row 0, then:
    cx.simulate_keystrokes("j e"); // row 1 is the ad hoc row
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "*".to_string()
        }
    );
    // The editor opens on the first dimension row, `lhu`; the next is `book`.
    cx.simulate_keystrokes("j space");
    cx.run_until_parked();
    assert_eq!(
        choice(&shell, &cx),
        GroupingChoice::AdHoc,
        "an ad hoc edit applies it"
    );
    assert_eq!(in_force(&shell, &cx), Some(chain(&["lhu", "book"])));
    assert!(
        is_open(&shell, &cx),
        "the editor stays open for the next tick"
    );
    flush_config_write(&mut cx);
    assert_eq!(written(&dir), "");
}

#[gpui::test]
fn the_last_ad_hoc_dimension_cannot_be_unticked(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog_after(cx, |f| {
        f.shared_mut().set_ad_hoc(chain(&["lhu"]));
    });
    cx.simulate_keystrokes("e space");
    cx.run_until_parked();
    assert_eq!(stored(&shell, &cx), Some(chain(&["lhu"])));
    assert!(
        !notice(&shell, &cx).is_empty(),
        "the refusal is said out loud"
    );
}

#[gpui::test]
fn d_forgets_the_ad_hoc_chain_from_the_list_without_asking(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog_after(cx, |f| {
        f.shared_mut().set_ad_hoc(chain(&["lhu"]));
    });
    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-confirm").is_none(),
        "a retypable chain needs no question"
    );
    assert_eq!(stored(&shell, &cx), None);
    assert_eq!(choice(&shell, &cx), GroupingChoice::ViewDefault);
    assert!(is_open(&shell, &cx));
}

#[gpui::test]
fn d_and_r_on_the_view_default_and_r_on_the_ad_hoc_row_are_refused(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog_after(cx, |f| {
        f.shared_mut().set_ad_hoc(chain(&["lhu"]));
        f.shared_mut().set_active_slot(None);
    });
    // Cursor opens on the view default (the active row).
    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    assert_eq!(
        notice(&shell, &cx),
        objectdialog::grouping_list::NOTHING_TO_CLEAR
    );
    assert_eq!(stored(&shell, &cx), Some(chain(&["lhu"])));
    cx.simulate_keystrokes("r");
    cx.run_until_parked();
    assert_eq!(
        notice(&shell, &cx),
        objectdialog::grouping_list::NOTHING_TO_REVERT
    );
    cx.simulate_keystrokes("j r");
    cx.run_until_parked();
    assert_eq!(
        notice(&shell, &cx),
        objectdialog::grouping_list::AD_HOC_NO_REVERT
    );
    assert_eq!(stored(&shell, &cx), Some(chain(&["lhu"])));
}

#[gpui::test]
fn ticking_in_an_empty_ad_hoc_editor_defines_the_chain(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, dir) = open_dialog(cx);
    cx.simulate_keystrokes("j e");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "*".to_string()
        }
    );
    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    assert_eq!(choice(&shell, &cx), GroupingChoice::AdHoc);
    assert_eq!(stored(&shell, &cx).map(|c| c.len()), Some(1));
    flush_config_write(&mut cx);
    assert_eq!(written(&dir), "");
}

#[gpui::test]
fn d_in_the_ad_hoc_editor_forgets_the_chain_and_returns_to_the_list(cx: &mut gpui::TestAppContext) {
    for route in ["key", "button"] {
        let (shell, mut cx, _dir) = open_dialog_after(cx, |f| {
            f.shared_mut().set_ad_hoc(chain(&["lhu"]));
        });
        cx.simulate_keystrokes("e");
        cx.run_until_parked();
        if route == "key" {
            cx.simulate_keystrokes("d");
        } else {
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            let button = cx
                .debug_bounds("objectdialog-action-d")
                .expect("the ad hoc editor offers its forget button");
            cx.simulate_click(button.center(), gpui::Modifiers::none());
        }
        cx.run_until_parked();
        assert_eq!(stored(&shell, &cx), None, "{route}");
        assert_eq!(choice(&shell, &cx), GroupingChoice::ViewDefault, "{route}");
        assert_eq!(
            dialog_state(&shell, &cx, |s| s.stage.clone()),
            objectdialog::Stage::Browse,
            "{route}"
        );
        assert!(
            cx.debug_bounds("objectdialog-confirm").is_none(),
            "{route}: no question"
        );
    }
}

#[gpui::test]
fn r_in_the_ad_hoc_editor_is_refused(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog_after(cx, |f| {
        f.shared_mut().set_ad_hoc(chain(&["lhu"]));
    });
    cx.simulate_keystrokes("e r");
    cx.run_until_parked();
    assert_eq!(
        notice(&shell, &cx),
        objectdialog::grouping_list::AD_HOC_NO_REVERT
    );
    assert_eq!(stored(&shell, &cx), Some(chain(&["lhu"])));
}

#[gpui::test]
fn an_untouched_seed_the_field_cannot_check_is_refused_not_applied(cx: &mut gpui::TestAppContext) {
    // The frame holds slot 2 as a chain no dataset column backs.
    let (shell, mut cx, _dir) = open_dialog_after(cx, |f| {
        f.stage_slot(2, chain(&["nope"]));
    });
    cx.simulate_keystrokes("j j j i"); // row 3 is slot 2
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(is_open(&shell, &cx), "the refusal keeps the field open");
    assert!(
        notice(&shell, &cx).contains("nope"),
        "{}",
        notice(&shell, &cx)
    );
    assert_eq!(stored(&shell, &cx), None);
    assert_eq!(choice(&shell, &cx), GroupingChoice::ViewDefault);
}

#[gpui::test]
fn an_untouched_ad_hoc_seed_the_field_cannot_check_is_refused_not_applied(
    cx: &mut gpui::TestAppContext,
) {
    // The lane stores an ad hoc chain no dataset column backs.
    let (shell, mut cx, _dir) = open_dialog_after(cx, |f| {
        f.shared_mut().set_ad_hoc(chain(&["nope"]));
    });
    let before = (stored(&shell, &cx), choice(&shell, &cx));
    assert_eq!(
        cursor_name(&shell, &cx).as_deref(),
        Some("*"),
        "the dialog opens on the active ad hoc row"
    );
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(is_open(&shell, &cx), "the refusal keeps the field open");
    assert!(edit_draft(&shell, &cx, |d| d.chain_entry()));
    assert!(
        notice(&shell, &cx).contains("nope"),
        "{}",
        notice(&shell, &cx)
    );
    assert_eq!((stored(&shell, &cx), choice(&shell, &cx)), before);
}

#[gpui::test]
fn a_failed_slot_write_leaves_an_open_ad_hoc_editor_as_it_was(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, dir) = open_dialog(cx);
    std::fs::write(dir.path().join("groupings.toml"), "3 = [\n").unwrap();
    // Tick `book` into slot 3 (queues a write), then open the ad hoc editor
    // before the write fails.
    cx.simulate_keystrokes("j j j j e j space escape");
    cx.run_until_parked();
    cx.simulate_keystrokes("k k k e");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "*".to_string()
        }
    );

    flush_config_write(&mut cx);

    assert!(
        edit_draft(&shell, &cx, |d| d.fields.iter().all(|f| f.key != "slot")),
        "the ad hoc draft contributed nothing to the batch and is not rebuilt as a slot"
    );
}

#[gpui::test]
fn the_dialog_edits_the_pinned_workspaces_own_ad_hoc_chain(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (window, mut vcx) = open_shell_with_user_dir(cx, services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    let frame = frame_of(&shell, &vcx);
    dispatch_action(&shell, "frame::pin_workspace", &mut vcx);
    vcx.run_until_parked();
    let ws = shell.read_with(&vcx, |s, _| s.target_frame().workspace());
    assert!(frame.read_with(&vcx, |f, _| f.is_pinned(ws)));

    dispatch_action(&shell, DOOR, &mut vcx);
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_keystrokes("i");
    vcx.run_until_parked();
    vcx.simulate_input("lhu");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();

    assert_eq!(
        frame.read_with(&vcx, |f, _| f.view(ws).ad_hoc().map(<[String]>::to_vec)),
        Some(chain(&["lhu"]))
    );
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().ad_hoc().map(<[String]>::to_vec)),
        None,
        "the shared lane is another lane"
    );
}
/// `services()` plus a user-layer `groupings` document defining slot 5.
/// The user document rides in the `builtin` list, as `services_with_views`
/// in `tests/objectdialog.rs` does: its `layer` field is what makes it the
/// user's.
fn services_with_a_user_slot_5() -> ShellServices {
    let mut services = services();
    let user = LayerDoc {
        layer: Layer::User,
        name: "groupings".to_string(),
        file: "<test:user>".into(),
        table: "5 = [\"book\"]\n".parse().unwrap(),
    };
    let mut docs = services.builtin.clone();
    docs.push(user);
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: docs,
        desk: None,
        user: None,
    });
    services
}

#[gpui::test]
fn s_then_a_digit_saves_the_ad_hoc_chain_to_an_empty_slot_and_activates_it(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, mut cx, dir) = open_dialog_after(cx, |f| {
        f.shared_mut().set_ad_hoc(chain(&["lhu", "book"]));
    });
    cx.simulate_keystrokes("s");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-save").is_some(),
        "the prompt replaces the action bar"
    );
    assert!(is_open(&shell, &cx));

    cx.simulate_keystrokes("6");
    cx.run_until_parked();
    assert!(!is_open(&shell, &cx));
    assert_eq!(choice(&shell, &cx), GroupingChoice::Slot(6));
    assert_eq!(in_force(&shell, &cx), Some(chain(&["lhu", "book"])));
    assert_eq!(
        stored(&shell, &cx),
        Some(chain(&["lhu", "book"])),
        "the ad hoc chain stays stored"
    );

    flush_config_write(&mut cx);
    assert!(
        written(&dir).contains("6 = [\"lhu\", \"book\"]"),
        "{}",
        written(&dir)
    );
}

#[gpui::test]
fn escape_cancels_the_save_prompt(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, dir) = open_dialog_after(cx, |f| {
        f.shared_mut().set_ad_hoc(chain(&["lhu"]));
    });
    cx.simulate_keystrokes("s escape");
    cx.run_until_parked();
    assert!(
        is_open(&shell, &cx),
        "escape answers the prompt, not the dialog"
    );
    assert!(cx.debug_bounds("objectdialog-save").is_none());
    cx.simulate_keystrokes("j"); // the list has its keys back
    cx.run_until_parked();
    assert_eq!(cursor_name(&shell, &cx).as_deref(), Some("1"));
    flush_config_write(&mut cx);
    assert_eq!(written(&dir), "");
}

#[gpui::test]
fn s_on_a_row_with_no_chain_is_refused(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog(cx);
    for keys in ["s", "j s", "j j j s"] {
        // the view default, the empty ad hoc row, empty slot 2
        cx.simulate_keystrokes(keys);
        cx.run_until_parked();
        assert!(cx.debug_bounds("objectdialog-save").is_none(), "{keys}");
        assert!(!notice(&shell, &cx).is_empty(), "{keys}");
        cx.simulate_keystrokes("g"); // back to the top row
        cx.run_until_parked();
    }
}

#[gpui::test]
fn saving_over_an_inherited_slot_forks_it_without_asking(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, dir) = open_dialog_after(cx, |f| {
        f.shared_mut().set_ad_hoc(chain(&["lhu", "book"]));
    });
    cx.simulate_keystrokes("s 3"); // slot 3 is builtin: lhu
    cx.run_until_parked();
    assert!(
        !is_open(&shell, &cx),
        "a recoverable fork is not a question"
    );
    assert_eq!(choice(&shell, &cx), GroupingChoice::Slot(3));
    assert_eq!(in_force(&shell, &cx), Some(chain(&["lhu", "book"])));
    let status = shell
        .read_with(&cx, |s, _| s.notice.clone())
        .unwrap_or_default();
    assert!(status.contains("copied '3'"), "{status}");
    flush_config_write(&mut cx);
    assert!(
        written(&dir).contains("3 = [\"lhu\", \"book\"]"),
        "{}",
        written(&dir)
    );
    let sidecar = std::fs::read_to_string(dir.path().join("overrides.toml")).unwrap_or_default();
    assert!(
        sidecar.contains("groupings.3"),
        "the fork's baseline is recorded: {sidecar}"
    );
}

#[gpui::test]
fn saving_over_a_user_owned_slot_asks_and_n_changes_nothing(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (window, mut cx) = open_shell_with_user_dir(cx, services_with_a_user_slot_5(), dir.path());
    let shell = shell_of(&window, &mut cx);
    let frame = frame_of(&shell, &cx);
    frame.update(&mut cx, |f, cx| {
        f.shared_mut().set_ad_hoc(chain(&["lhu"]));
        cx.notify();
    });
    cx.run_until_parked();
    dispatch_action(&shell, DOOR, &mut cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    cx.simulate_keystrokes("s 5");
    cx.run_until_parked();
    assert!(
        is_open(&shell, &cx),
        "a user-owned chain would be lost: ask"
    );
    assert!(cx.debug_bounds("objectdialog-save-replace").is_some());

    cx.simulate_keystrokes("n");
    cx.run_until_parked();
    assert!(is_open(&shell, &cx));
    assert!(
        cx.debug_bounds("objectdialog-save").is_none(),
        "the prompt is gone"
    );
    assert_eq!(choice(&shell, &cx), GroupingChoice::AdHoc);
    assert_eq!(
        frame.read_with(&cx, |f, _| f.slots().get(5).map(<[String]>::to_vec)),
        Some(chain(&["book"]))
    );
    flush_config_write(&mut cx);
    assert_eq!(written(&dir), "");

    cx.simulate_keystrokes("s 5 y");
    cx.run_until_parked();
    assert!(!is_open(&shell, &cx));
    assert_eq!(choice(&shell, &cx), GroupingChoice::Slot(5));
    assert_eq!(in_force(&shell, &cx), Some(chain(&["lhu"])));
}

#[gpui::test]
fn saving_a_chain_a_slot_already_holds_writes_nothing_and_activates_it(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, mut cx, dir) = open_dialog_after(cx, |f| {
        f.shared_mut().set_ad_hoc(chain(&["lhu"]));
    });
    cx.simulate_keystrokes("s");
    cx.run_until_parked();
    // Without the prompt, `3` would apply slot 3 from the list and pass.
    assert!(cx.debug_bounds("objectdialog-save").is_some());
    cx.simulate_keystrokes("3"); // slot 3 already holds lhu
    cx.run_until_parked();
    assert!(!is_open(&shell, &cx));
    assert_eq!(choice(&shell, &cx), GroupingChoice::Slot(3));
    flush_config_write(&mut cx);
    assert_eq!(
        written(&dir),
        "",
        "an equal chain is not a write, and not a fork"
    );
}

#[gpui::test]
fn mod_s_in_the_list_opened_chain_field_saves_the_typed_chain(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, dir) = open_dialog(cx);
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    cx.simulate_input("book lhu");
    cx.simulate_keystrokes("alt-s");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-save").is_some());
    cx.simulate_keystrokes("7");
    cx.run_until_parked();
    assert!(!is_open(&shell, &cx));
    assert_eq!(choice(&shell, &cx), GroupingChoice::Slot(7));
    assert_eq!(
        stored(&shell, &cx),
        None,
        "the typed chain went to the slot, not the ad hoc store"
    );
    flush_config_write(&mut cx);
    assert!(
        written(&dir).contains("7 = [\"book\", \"lhu\"]"),
        "{}",
        written(&dir)
    );
}

#[gpui::test]
fn mod_s_on_a_refused_chain_keeps_the_field_open(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog(cx);
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    cx.simulate_input("nope");
    cx.simulate_keystrokes("alt-s");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-save").is_none());
    assert!(edit_draft(&shell, &cx, |d| d.chain_entry()));
    assert!(notice(&shell, &cx).contains("nope"));
}

#[gpui::test]
fn s_in_an_edit_stage_saves_that_stages_chain(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, dir) = open_dialog(cx);
    cx.simulate_keystrokes("j j e"); // slot 1's editor: book / lhu
    cx.run_until_parked();
    cx.simulate_keystrokes("s 8");
    cx.run_until_parked();
    assert!(!is_open(&shell, &cx));
    assert_eq!(choice(&shell, &cx), GroupingChoice::Slot(8));
    flush_config_write(&mut cx);
    assert!(
        written(&dir).contains("8 = [\"book\", \"lhu\"]"),
        "{}",
        written(&dir)
    );
}

#[gpui::test]
fn a_click_on_a_prompt_digit_saves_to_that_slot(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog_after(cx, |f| {
        f.shared_mut().set_ad_hoc(chain(&["lhu", "book"]));
    });
    cx.simulate_keystrokes("s");
    cx.run_until_parked();
    let digit = cx
        .debug_bounds("objectdialog-save-9")
        .expect("the digit is a button");
    cx.simulate_click(digit.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(!is_open(&shell, &cx));
    assert_eq!(choice(&shell, &cx), GroupingChoice::Slot(9));
}

#[gpui::test]
fn a_row_click_under_the_save_prompt_is_ignored(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog_after(cx, |f| {
        f.shared_mut().set_ad_hoc(chain(&["lhu"]));
    });
    cx.simulate_keystrokes("s");
    cx.run_until_parked();
    let row = cx.debug_bounds("objectdialog-row-1").unwrap();
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(40.0), row.origin.y + gpui::px(4.0)),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert!(
        is_open(&shell, &cx),
        "a question owns the pointer as well as the keys"
    );
    assert_eq!(choice(&shell, &cx), GroupingChoice::AdHoc);
}

#[gpui::test]
fn a_click_on_replace_saves_over_a_user_owned_slot(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (window, mut cx) = open_shell_with_user_dir(cx, services_with_a_user_slot_5(), dir.path());
    let shell = shell_of(&window, &mut cx);
    let frame = frame_of(&shell, &cx);
    frame.update(&mut cx, |f, cx| {
        f.shared_mut().set_ad_hoc(chain(&["lhu"]));
        cx.notify();
    });
    cx.run_until_parked();
    dispatch_action(&shell, DOOR, &mut cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_keystrokes("s 5");
    cx.run_until_parked();
    let yes = cx
        .debug_bounds("objectdialog-save-confirm-yes")
        .expect("the question's answers are buttons");
    cx.simulate_click(yes.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(!is_open(&shell, &cx));
    assert_eq!(choice(&shell, &cx), GroupingChoice::Slot(5));
    assert_eq!(in_force(&shell, &cx), Some(chain(&["lhu"])));
}

#[gpui::test]
fn a_filter_row_click_under_the_save_prompt_is_ignored(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog_after(cx, |f| {
        f.shared_mut().set_ad_hoc(chain(&["lhu"]));
    });
    cx.simulate_keystrokes("s");
    cx.run_until_parked();
    let row = cx
        .debug_bounds("dialog-filter-frozen")
        .expect("the list's filter row is frozen in normal mode");
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(20.0), row.origin.y + gpui::px(4.0)),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-save").is_some(),
        "the prompt is still up"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.mode),
        crate::dialogmode::DialogMode::Normal
    );
}

#[gpui::test]
fn a_back_click_under_the_save_prompt_is_ignored(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog(cx);
    cx.simulate_keystrokes("j j e s"); // slot 1's editor, then the prompt
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-save").is_some());
    let back = cx
        .debug_bounds("shell-modal-back")
        .expect("the Back button paints in an edit stage");
    cx.simulate_click(back.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("objectdialog-save").is_some(),
        "the prompt is still up"
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "1".to_string()
        }
    );
}

#[gpui::test]
fn saving_over_a_slot_the_pending_batch_just_forked_asks(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog_after(cx, |f| {
        f.shared_mut().set_ad_hoc(chain(&["position_ref"]));
    });
    // The cursor opens on the active `*` row: `j` is slot 1 (builtin).
    cx.simulate_keystrokes("j e j space");
    cx.run_until_parked();
    assert!(
        shell.read_with(&cx, |s, _| s.pending_config_write.is_some()),
        "the tick queued a fork of slot 1, not yet promoted"
    );
    cx.simulate_keystrokes("escape k");
    cx.run_until_parked();
    assert_eq!(cursor_name(&shell, &cx).as_deref(), Some("*"));
    cx.simulate_keystrokes("s 1");
    cx.run_until_parked();
    assert!(is_open(&shell, &cx), "slot 1 is the user's now: ask");
    assert!(cx.debug_bounds("objectdialog-save-replace").is_some());
}

#[gpui::test]
fn saving_the_chain_a_pending_edit_just_gave_a_slot_activates_it(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog_after(cx, |f| {
        f.shared_mut().set_ad_hoc(chain(&["lhu"]));
    });
    // Define empty slot 2 by ticking its first dimension, `lhu`, without
    // waiting for the write: the frame does not hold slot 2 yet.
    cx.simulate_keystrokes("j j e j space escape");
    cx.run_until_parked();
    assert!(shell.read_with(&cx, |s, _| s.pending_config_write.is_some()));
    cx.simulate_keystrokes("k k");
    cx.run_until_parked();
    assert_eq!(cursor_name(&shell, &cx).as_deref(), Some("*"));
    cx.simulate_keystrokes("s 2");
    cx.run_until_parked();
    assert!(!is_open(&shell, &cx), "an equal chain is not a question");
    assert_eq!(
        choice(&shell, &cx),
        GroupingChoice::Slot(2),
        "the slot the pending batch defines is activated, not refused"
    );
    assert_eq!(in_force(&shell, &cx), Some(chain(&["lhu"])));
}

#[gpui::test]
fn mod_s_on_an_untouched_seed_the_field_cannot_check_keeps_the_field_open(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, mut cx, _dir) = open_dialog_after(cx, |f| {
        f.shared_mut().set_ad_hoc(chain(&["nope"]));
    });
    assert_eq!(cursor_name(&shell, &cx).as_deref(), Some("*"));
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    cx.simulate_keystrokes("alt-s");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-save").is_none());
    assert!(
        edit_draft(&shell, &cx, |d| d.chain_entry()),
        "the field is still open"
    );
    assert!(
        notice(&shell, &cx).contains("nope"),
        "{}",
        notice(&shell, &cx)
    );
}

#[gpui::test]
fn a_save_with_no_user_directory_is_refused_and_changes_nothing(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell_with(cx, services(), DOOR);
    cx.simulate_keystrokes("j j s 6"); // slot 1 holds book / lhu
    cx.run_until_parked();
    assert!(is_open(&shell, &cx));
    assert!(
        notice(&shell, &cx).contains("user config directory"),
        "{}",
        notice(&shell, &cx)
    );
    assert_eq!(
        frame_of(&shell, &cx).read_with(&cx, |f, _| f.slots().get(6).map(<[String]>::to_vec)),
        None
    );
    assert_eq!(choice(&shell, &cx), GroupingChoice::ViewDefault);
}

#[gpui::test]
fn escape_from_a_mod_s_prompt_returns_to_the_chain_field(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, dir) = open_dialog(cx);
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    cx.simulate_input("book lhu");
    cx.simulate_keystrokes("alt-s");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-save").is_some());
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-save").is_none());
    assert!(
        edit_draft(&shell, &cx, |d| d.chain_entry()),
        "back in the field"
    );
    assert_eq!(edit_draft(&shell, &cx, |d| d.query.clone()), "book / lhu");
    // The field's own escape is the list-opened one: back to the list,
    // nothing applied.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    );
    assert_eq!(stored(&shell, &cx), None);
    flush_config_write(&mut cx);
    assert_eq!(written(&dir), "");
}

#[gpui::test]
fn the_edit_control_opens_that_rows_editor_without_applying_it(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog(cx);
    let edit = cx
        .debug_bounds("objectdialog-row-edit-3")
        .expect("slot 3's edit control is painted");
    cx.simulate_click(edit.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(is_open(&shell, &cx));
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "3".to_string()
        }
    );
    assert_eq!(
        choice(&shell, &cx),
        GroupingChoice::ViewDefault,
        "the press reached the control, not the row beneath it"
    );
    // A stage opened by the mouse still takes keys.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    );
}

#[gpui::test]
fn the_save_control_asks_for_a_slot_for_that_rows_chain(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog(cx);
    let save = cx
        .debug_bounds("objectdialog-row-save-1")
        .expect("slot 1's save control is painted");
    cx.simulate_click(save.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(is_open(&shell, &cx));
    assert_eq!(
        dialog_state(&shell, &cx, |s| s
            .save
            .as_ref()
            .map(|save| save.chain.clone())),
        Some(chain(&["book", "lhu"]))
    );
    cx.simulate_keystrokes("9");
    cx.run_until_parked();
    assert_eq!(choice(&shell, &cx), GroupingChoice::Slot(9));
}

#[gpui::test]
fn rows_without_a_chain_offer_no_save_and_the_view_default_offers_neither(
    cx: &mut gpui::TestAppContext,
) {
    let (_shell, mut cx, _dir) = open_dialog(cx);
    assert!(cx.debug_bounds("objectdialog-row-edit-0").is_none());
    assert!(cx.debug_bounds("objectdialog-row-save-0").is_none());
    assert!(
        cx.debug_bounds("objectdialog-row-edit-2").is_some(),
        "an empty slot can be edited"
    );
    assert!(
        cx.debug_bounds("objectdialog-row-save-2").is_none(),
        "but has no chain to save"
    );
    assert!(cx.debug_bounds("objectdialog-row-edit-*").is_some());
    assert!(cx.debug_bounds("objectdialog-row-save-*").is_none());
}

#[gpui::test]
fn a_slot_the_reader_dropped_offers_edit_but_no_save(cx: &mut gpui::TestAppContext) {
    // Slot 4 is defined in the user layer with a column no dataset declares:
    // the row has a layer, but the frame holds no chain for it, so `s`
    // would refuse and the pointer must not offer it either.
    let mut services = services();
    let user = LayerDoc {
        layer: Layer::User,
        name: "groupings".to_string(),
        file: "<test:user>".into(),
        table: "4 = [\"nope\"]\n".parse().unwrap(),
    };
    let mut docs = services.builtin.clone();
    docs.push(user);
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: docs,
        desk: None,
        user: None,
    });
    let dir = tempfile::tempdir().unwrap();
    let (window, mut cx) = open_shell_with_user_dir(cx, services, dir.path());
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, DOOR, &mut cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        dialog_state(&shell, &cx, |s| s
            .rows
            .rows()
            .iter()
            .any(|r| r.name == "4" && r.layer == Some(Layer::User))),
        "the configuration defines slot 4"
    );
    assert!(cx.debug_bounds("objectdialog-row-edit-4").is_some());
    assert!(cx.debug_bounds("objectdialog-row-save-4").is_none());
}

#[gpui::test]
fn a_row_control_click_under_the_save_prompt_is_ignored(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, _dir) = open_dialog_after(cx, |f| {
        f.shared_mut().set_ad_hoc(chain(&["lhu"]));
    });
    cx.simulate_keystrokes("s");
    cx.run_until_parked();
    let edit = cx
        .debug_bounds("objectdialog-row-edit-3")
        .expect("slot 3's edit control is painted under the prompt");
    cx.simulate_click(edit.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| (
            s.stage.clone(),
            s.save.as_ref().map(|save| save.chain.clone())
        )),
        (objectdialog::Stage::Browse, Some(chain(&["lhu"]))),
        "a question owns the pointer as well as the keys"
    );
    assert_eq!(choice(&shell, &cx), GroupingChoice::AdHoc);
}
