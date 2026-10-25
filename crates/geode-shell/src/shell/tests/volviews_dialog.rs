//! The Vol views object dialog through real key dispatch: the tabs listed in
//! document order, a catalogue row added with `space` and written to the user
//! layer as a fork of the builtin tab, a taken name refused at the naming
//! prompt, and a new tab's first write.

use super::objectdialog::{dialog_state, edit_draft, flush_config_write};
use super::*;
use crate::shell::objectdialog::Stage;

/// Two tabs: `vol` (the default, two columns) and `skew` (single cells, one
/// slider column) — the pure adapter tests' fixture, over an `ssr` dataset
/// keyed by underlying and tenor so the catalogue ends with `SSR`.
const BUILTIN: &str = "config_version = 1\ndefault = \"vol\"\n[vol]\n[[vol.columns]]\nmeasure = \"atfm 3M\"\n[[vol.columns]]\nkind = \"vega\"\n[skew]\ncells = \"single\"\n[[skew.columns]]\nmeasure = \"skew 1Y\"\ncompanion = \"slider\"\n";
const SSR_DATASET: &str = "[ssr]\nfamily = \"history\"\nkey = [\"underlying_ref\", \"tenor\"]\nhistory = \"800d\"\n[ssr.columns.underlying_ref]\ntype = \"utf8\"\nrole = \"dimension\"\n[ssr.columns.tenor]\ntype = \"utf8\"\nrole = \"dimension\"\n[ssr.columns.ssr]\ntype = \"f64\"\nrole = \"value\"\n";

fn services_with_vol_views() -> ShellServices {
    let mut services = test_services();
    let builtin = LayerDoc::builtin("vol_views", BUILTIN).unwrap();
    let datasets = LayerDoc::builtin("datasets", SSR_DATASET).unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![builtin, datasets],
        desk: None,
        user: None,
    });
    services
}

/// `config::vol_views` lists the tabs in document order, in normal mode.
#[gpui::test]
fn config_vol_views_lists_the_tabs(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) =
        dialog_test_shell_with(cx, services_with_vol_views(), "config::vol_views");
    cx.run_until_parked();
    let names = dialog_state(&shell, &cx, |s| {
        s.rows
            .rows()
            .iter()
            .map(|r| r.name.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(names, ["vol", "skew"]);
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.mode),
        crate::dialogmode::DialogMode::Normal
    );
    assert!(cx.debug_bounds("objectdialog-row-vol").is_some());
    assert!(
        cx.debug_bounds("objectdialog-row-default").is_none(),
        "the document's `default` key is not a tab"
    );
}

/// `space` on the SSR catalogue row appends an `SSR 1W` column, the debounced
/// write lands in the user layer, and the builtin tab forks with its notice.
#[gpui::test]
fn space_adds_a_dataset_column_and_the_write_forks_the_builtin_tab(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_vol_views(),
        dir.path(),
        "config::vol_views",
    );
    cx.simulate_keystrokes("enter"); // vol
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        Stage::Edit { object } if object == "vol"
    ));
    cx.simulate_keystrokes("shift-g"); // last row: the catalogue's SSR
    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    let items = edit_draft(&shell, &cx, |d| {
        d.list_items("columns")
            .unwrap()
            .iter()
            .map(|i| i.name.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(items, ["ATFM 3M", "Book vega", "SSR 1W"]);
    flush_config_write(&mut cx);
    let text = std::fs::read_to_string(dir.path().join("vol_views.toml")).unwrap();
    assert!(text.contains("measure = \"ssr 1W\""), "{text}");
    assert!(text.contains("measure = \"atfm 3M\""), "{text}");
    assert!(
        !text.contains("default"),
        "the document's key is not written with the view: {text}"
    );
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        notice
            .as_deref()
            .is_some_and(|n| n.starts_with("copied 'vol' to your config")),
        "{notice:?}"
    );
}

/// Naming a new tab after one that exists is refused at the prompt and
/// nothing is written. (The scaffold has no rename path: the edit stage's
/// `name` row is display-only, so the naming prompt is the one door a
/// duplicate can be typed through.)
#[gpui::test]
fn naming_a_tab_after_an_existing_one_is_refused(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_vol_views(),
        dir.path(),
        "config::vol_views",
    );
    cx.simulate_keystrokes("n");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        Stage::Naming
    );
    cx.simulate_input("vol");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some("'vol' already exists — open it instead")
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        Stage::Naming
    );
    flush_config_write(&mut cx);
    assert!(!dir.path().join("vol_views.toml").exists());
}

/// A reserved name is refused the same way: `default` is the document's key.
#[gpui::test]
fn naming_a_tab_default_is_refused(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_vol_views(),
        dir.path(),
        "config::vol_views",
    );
    cx.simulate_keystrokes("n");
    cx.simulate_input("default");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some("'default' is reserved")
    );
    flush_config_write(&mut cx);
    assert!(!dir.path().join("vol_views.toml").exists());
}

/// `n` names a new tab; it opens empty with pair cells and writes `columns = []`.
#[gpui::test]
fn a_new_tab_opens_empty_and_writes_an_empty_column_list(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_vol_views(),
        dir.path(),
        "config::vol_views",
    );
    cx.simulate_keystrokes("n");
    cx.simulate_input("analytics");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        Stage::Edit { object } if object == "analytics"
    ));
    assert!(edit_draft(&shell, &cx, |d| d.is_new));
    assert!(edit_draft(&shell, &cx, |d| d
        .list_items("columns")
        .unwrap()
        .is_empty()));
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.choice("cells").map(str::to_string)),
        Some("pair".into())
    );
    flush_config_write(&mut cx);
    let text = std::fs::read_to_string(dir.path().join("vol_views.toml")).unwrap();
    assert!(
        text.contains("[analytics]") && text.contains("columns = []"),
        "{text}"
    );
}

/// A name differing only in case from an existing tab is the reader's
/// duplicate (`from_doc` skips the second), so the naming prompt refuses it
/// and nothing reaches the user file.
#[gpui::test]
fn naming_a_tab_in_another_case_of_an_existing_one_is_refused(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_vol_views(),
        dir.path(),
        "config::vol_views",
    );
    cx.simulate_keystrokes("n");
    cx.simulate_input("Vol");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some("'Vol' already exists — open it instead")
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        Stage::Naming
    );
    flush_config_write(&mut cx);
    assert!(!dir.path().join("vol_views.toml").exists());
}

/// `enter` on a column row opens its stage; stepping Companion to `bar`
/// renames the row (and the stage with it), a Family step to Forward keeps
/// the glyph, and leaving writes `companion = "bar"` under that column.
#[gpui::test]
fn the_column_stage_edits_a_glyph_and_the_file_follows(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_vol_views(),
        dir.path(),
        "config::vol_views",
    );
    cx.simulate_keystrokes("enter"); // vol
    cx.run_until_parked();
    // The cursor settles on `cells` (`name` is display-only); `default`, then
    // past the list header to the first column, ATFM 3M.
    cx.simulate_keystrokes("j j enter");
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        Stage::Column { object, column } if object == "vol" && column == "ATFM 3M"
    ));
    // Companion is the fourth row (family, tenor, level, companion); `space`
    // steps it default → chip → bar, each step folding and renaming the row.
    cx.simulate_keystrokes("j j j space space");
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        Stage::Column { column, .. } if column == "ATFM 3M · bar"
    ));
    // Back on Family, one step: ATFM → Forward. The glyph is the column's,
    // not the family's, so it comes along.
    cx.simulate_keystrokes("k k k space");
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        Stage::Column { column, .. } if column == "Forward 3M · bar"
    ));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        Stage::Edit { object } if object == "vol"
    ));
    let items = edit_draft(&shell, &cx, |d| {
        d.list_items("columns")
            .unwrap()
            .iter()
            .map(|i| i.name.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(items, ["Forward 3M · bar", "Book vega"]);
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.row_label(d.selected_row().unwrap())),
        "Forward 3M · bar",
        "the cursor returns to the column just edited, under its new name"
    );
    flush_config_write(&mut cx);
    let text = std::fs::read_to_string(dir.path().join("vol_views.toml")).unwrap();
    assert!(text.contains("companion = \"bar\""), "{text}");
    assert!(text.contains("measure = \"forward 3M\""), "{text}");
    assert!(
        !text.contains("level") && !text.contains("label"),
        "only the set key reaches the file: {text}"
    );
}

/// `shift+space` on Family wraps from ATFM to the catalogue's last family, the
/// `ssr` dataset: the stage re-derives its rows for the new family (the tenor
/// kept), is re-keyed to the new head, and the file names the dataset exactly.
#[gpui::test]
fn a_family_change_re_derives_the_stage_and_keeps_the_tenor(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_vol_views(),
        dir.path(),
        "config::vol_views",
    );
    cx.simulate_keystrokes("enter"); // vol
    cx.run_until_parked();
    cx.simulate_keystrokes("j j enter"); // ATFM 3M
    cx.run_until_parked();
    cx.simulate_keystrokes("shift-space"); // Family: ATFM → SSR (wraps)
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        Stage::Column { column, .. } if column == "SSR 3M"
    ));
    let keys = edit_draft(&shell, &cx, |d| {
        d.fields.iter().map(|f| f.key.clone()).collect::<Vec<_>>()
    });
    assert_eq!(
        keys,
        ["family", "tenor", "level", "companion", "label", "group"]
    );
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.choice("tenor").map(str::to_string)),
        Some("3M".into())
    );
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.text("group").map(str::to_string)),
        Some("SSR".into()),
        "the group row is prefilled with the new family's default"
    );
    flush_config_write(&mut cx);
    let text = std::fs::read_to_string(dir.path().join("vol_views.toml")).unwrap();
    assert!(text.contains("measure = \"ssr 3M\""), "{text}");
    assert!(!text.contains("atfm 3M"), "{text}");
}
