//! The VolMonitor views object dialog through real key dispatch: the tabs listed in
//! document order, a catalogue row added with `space` and written to the user
//! layer as a fork of the builtin tab, a taken name refused at the naming
//! prompt, and a new tab's first write.

use super::objectdialog::{dialog_state, edit_draft, flush_config_write};
use super::*;
use crate::defaults::AddPlacement;
use crate::module::ConfigObject;
use crate::module::recording::RecordingFactory;
use crate::shell::objectdialog::Stage;
use geode_core::config::Severity;

/// Two tabs: `vol` (the default, two columns) and `skew` (single cells, one
/// slider column) — the pure adapter tests' fixture, over an `ssr` dataset
/// keyed by underlying and tenor so the catalogue ends with `SSR`.
const BUILTIN: &str = "config_version = 1\ndefault = \"vol\"\n[vol]\n[[vol.columns]]\nmeasure = \"atfm 3M\"\n[[vol.columns]]\nkind = \"vega\"\n[skew]\ncells = \"single\"\n[[skew.columns]]\nmeasure = \"skew 1Y\"\ncompanion = \"slider\"\n";
const SSR_DATASET: &str = "[ssr]\nfamily = \"history\"\nkey = [\"underlying_ref\", \"tenor\"]\nhistory = \"800d\"\n[ssr.columns.underlying_ref]\ntype = \"utf8\"\nrole = \"dimension\"\n[ssr.columns.tenor]\ntype = \"utf8\"\nrole = \"dimension\"\n[ssr.columns.ssr]\ntype = \"f64\"\nrole = \"value\"\n";

/// The four shipped tabs by name, one measure column each, `vol` the default.
const FOUR_TABS: &str = "config_version = 1\ndefault = \"vol\"\n[vol]\n[[vol.columns]]\nmeasure = \"atfm 3M\"\n[skew]\n[[skew.columns]]\nmeasure = \"skew 1Y\"\n[varswap]\n[[varswap.columns]]\nmeasure = \"varswap 3M\"\n[surface]\n[[surface.columns]]\nmeasure = \"atfm 1Y\"\n";

fn services_with_vol_views() -> ShellServices {
    services_over(BUILTIN)
}

/// [`services_with_vol_views`] over another builtin `vol_views.toml`.
fn services_over(vol_views: &str) -> ShellServices {
    let mut services = test_services();
    let builtin = LayerDoc::builtin("vol_views", vol_views).unwrap();
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

/// The new tab's Name row shows the name the prompt committed, not a blank
/// the tab would only fill once reopened.
#[gpui::test]
fn a_new_tab_shows_its_name_in_the_view_stage(cx: &mut gpui::TestAppContext) {
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
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.text("name").map(str::to_string)),
        Some("analytics".into())
    );
    assert!(
        edit_draft(&shell, &cx, |d| !d.is_dirty()),
        "the seeded name is the baseline, not an edit"
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

/// [`services_with_vol_views`] over a user layer read from `dir`, the
/// directory the test also hands the shell to write into — so what the
/// dialog deletes is a tab the file really holds.
fn services_with_user_vol_views(dir: &std::path::Path) -> ShellServices {
    let mut services = test_services();
    let builtin = LayerDoc::builtin("vol_views", BUILTIN).unwrap();
    let datasets = LayerDoc::builtin("datasets", SSR_DATASET).unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![builtin, datasets],
        desk: None,
        user: Some(dir.to_path_buf()),
    });
    services
}

/// `shift+j` on the browse list moves a tab down: the list shows the move at
/// once, the user file holds the new order with both builtin tabs copied
/// into it, and one notice reports the copies.
#[gpui::test]
fn shift_j_on_the_browse_list_reorders_the_tabs_and_writes_them(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_vol_views(),
        dir.path(),
        "config::vol_views",
    );
    cx.simulate_keystrokes("shift-j");
    cx.run_until_parked();
    let names = dialog_state(&shell, &cx, |s| {
        s.rows
            .rows()
            .iter()
            .map(|r| r.name.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(names, ["skew", "vol"]);
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.selected),
        1,
        "the cursor follows the tab"
    );
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert_eq!(
        notice.as_deref(),
        Some("copied 2 tabs to your config — r restores each")
    );
    flush_config_write(&mut cx);
    let names = dialog_state(&shell, &cx, |s| {
        s.rows
            .rows()
            .iter()
            .map(|r| r.name.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        names,
        ["skew", "vol"],
        "the rows re-derived from the applied configuration hold the order"
    );
    assert_eq!(dialog_state(&shell, &cx, |s| s.selected), 1);
    let text = std::fs::read_to_string(dir.path().join("vol_views.toml")).unwrap();
    let skew = text.find("[skew]").unwrap();
    let vol = text.find("[vol]").unwrap();
    assert!(skew < vol, "{text}");
    assert!(
        !text.contains("default"),
        "the document's key is not the move's to write: {text}"
    );
    let overrides = std::fs::read_to_string(dir.path().join("overrides.toml")).unwrap();
    assert!(
        overrides.contains("vol_views.vol") && overrides.contains("vol_views.skew"),
        "each copy is recorded so r can restore it: {overrides}"
    );
}

/// `shift+k` on the first tab has nowhere to go: nothing moves, nothing is
/// written, and the footer says so.
#[gpui::test]
fn shift_k_on_the_first_tab_is_refused_and_writes_nothing(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_vol_views(),
        dir.path(),
        "config::vol_views",
    );
    cx.simulate_keystrokes("shift-k");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some("that is as far as this tab goes")
    );
    flush_config_write(&mut cx);
    assert!(!dir.path().join("vol_views.toml").exists());
}

/// The four shipped tabs. A glyph edit forks `surface`, then another forks
/// `vol`: the user file holds both copies and the merged order — the
/// browse rows, the tile's digits — stays `vol skew varswap surface`. A
/// keyed write appends the key the file lacks, so without the batch's order
/// rewrite the second copy would follow the first and the merge, which
/// takes the user layer's order for the tabs it holds, would read
/// `surface skew varswap vol` with no move asked.
#[gpui::test]
fn an_edit_on_a_later_tab_does_not_reorder_the_tabs(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_over(FOUR_TABS),
        dir.path(),
        "config::vol_views",
    );
    let names = |shell: &Entity<ShellView>, cx: &gpui::VisualTestContext| {
        dialog_state(shell, cx, |s| {
            s.rows
                .rows()
                .iter()
                .map(|r| r.name.clone())
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(names(&shell, &cx), ["vol", "skew", "varswap", "surface"]);
    // surface: its first column's stage, Companion (family, tenor, level,
    // companion) stepped to chip, then back to the browse list.
    cx.simulate_keystrokes("shift-g enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("j j enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("j j j space");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape escape");
    cx.run_until_parked();
    flush_config_write(&mut cx);
    assert_eq!(names(&shell, &cx), ["vol", "skew", "varswap", "surface"]);
    // vol, the same edit.
    cx.simulate_keystrokes("k k k enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("j j enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("j j j space");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape escape");
    cx.run_until_parked();
    flush_config_write(&mut cx);
    assert_eq!(
        names(&shell, &cx),
        ["vol", "skew", "varswap", "surface"],
        "a fork lands at its merged position; only a move changes the order"
    );
    let text = std::fs::read_to_string(dir.path().join("vol_views.toml")).unwrap();
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle}: {text}"))
    };
    assert!(at("[vol]") < at("[surface]"), "{text}");
    assert_eq!(text.matches("companion = \"chip\"").count(), 2, "{text}");
    assert!(
        !text.contains("[skew]") && !text.contains("[varswap]"),
        "the tabs between were not crossed and are not copied: {text}"
    );
}

/// A hand-written column the reader refuses (`measure = "atfm 3m x"`) is
/// carried through the dialog as it was read: a `Cells` edit on its tab
/// forks the tab and the refused column is still in the file.
#[gpui::test]
fn a_refused_column_survives_a_save(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let builtin = "config_version = 1\n[vol]\n[[vol.columns]]\nmeasure = \"atfm 3m x\"\n[[vol.columns]]\nmeasure = \"atfm 3M\"\n";
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_over(builtin), dir.path(), "config::vol_views");
    cx.simulate_keystrokes("enter"); // vol
    cx.run_until_parked();
    let items = edit_draft(&shell, &cx, |d| {
        d.list_items("columns")
            .unwrap()
            .iter()
            .map(|i| i.name.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(items, ["atfm 3m x", "ATFM 3M"]);
    cx.simulate_keystrokes("space"); // Cells: pair → single
    cx.run_until_parked();
    flush_config_write(&mut cx);
    let text = std::fs::read_to_string(dir.path().join("vol_views.toml")).unwrap();
    assert!(text.contains("cells = \"single\""), "{text}");
    assert!(text.contains("measure = \"atfm 3m x\""), "{text}");
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle}: {text}"))
    };
    assert!(at("atfm 3m x") < at("measure = \"atfm 3M\""), "{text}");
}

/// `space` on Default for the tab a lower layer's `default` names is
/// refused: the user file has no key to remove, so the field would snap
/// back with nothing written. Nothing reaches the file.
#[gpui::test]
fn un_defaulting_the_shipped_default_is_refused(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_vol_views(),
        dir.path(),
        "config::vol_views",
    );
    cx.simulate_keystrokes("enter"); // vol, the builtin default
    cx.run_until_parked();
    cx.simulate_keystrokes("j space"); // Default
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some("vol is the shipped default — make another tab the default instead")
    );
    assert!(edit_draft(&shell, &cx, |d| matches!(
        d.fields[2].kind,
        crate::shell::objectdialog::FieldKind::Bool(true)
    )));
    flush_config_write(&mut cx);
    assert!(!dir.path().join("vol_views.toml").exists());
}

/// A `default` the user layer holds is the user's to remove: Default off
/// on that tab clears the key.
#[gpui::test]
fn un_defaulting_a_user_default_removes_the_key(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let user =
        "config_version = 1\ndefault = \"mine\"\n[mine]\n[[mine.columns]]\nkind = \"vega\"\n";
    std::fs::write(dir.path().join("vol_views.toml"), user).unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_user_vol_views(dir.path()),
        dir.path(),
        "config::vol_views",
    );
    cx.simulate_keystrokes("shift-g enter"); // mine
    cx.run_until_parked();
    cx.simulate_keystrokes("j space"); // Default
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| matches!(
        d.fields[2].kind,
        crate::shell::objectdialog::FieldKind::Bool(false)
    )));
    flush_config_write(&mut cx);
    let text = std::fs::read_to_string(dir.path().join("vol_views.toml")).unwrap();
    assert!(!text.contains("default"), "{text}");
    assert!(text.contains("[mine]"), "{text}");
}

/// Two moves before one flush: `shift+j` twice from `vol` on three tabs
/// writes the final order once, and the second move's notice names only
/// the tab it crossed that the first had not already copied.
#[gpui::test]
fn two_moves_before_a_flush_write_the_final_order(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let three = "config_version = 1\n[vol]\n[[vol.columns]]\nmeasure = \"atfm 3M\"\n[skew]\n[[skew.columns]]\nmeasure = \"skew 1Y\"\n[varswap]\n[[varswap.columns]]\nmeasure = \"varswap 3M\"\n";
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_over(three), dir.path(), "config::vol_views");
    cx.simulate_keystrokes("shift-j");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some("copied 2 tabs to your config — r restores each")
    );
    cx.simulate_keystrokes("shift-j");
    cx.run_until_parked();
    let names = |shell: &Entity<ShellView>, cx: &gpui::VisualTestContext| {
        dialog_state(shell, cx, |s| {
            s.rows
                .rows()
                .iter()
                .map(|r| r.name.clone())
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(names(&shell, &cx), ["skew", "varswap", "vol"]);
    assert_eq!(dialog_state(&shell, &cx, |s| s.selected), 2);
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert_eq!(
        notice.as_deref(),
        Some("copied 'varswap' to your config — r restores the builtin copy"),
        "vol was already the user's after the first move"
    );
    flush_config_write(&mut cx);
    assert_eq!(names(&shell, &cx), ["skew", "varswap", "vol"]);
    let text = std::fs::read_to_string(dir.path().join("vol_views.toml")).unwrap();
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle}: {text}"))
    };
    assert!(at("[skew]") < at("[varswap]"), "{text}");
    assert!(at("[varswap]") < at("[vol]"), "{text}");
    let overrides = std::fs::read_to_string(dir.path().join("overrides.toml")).unwrap();
    for tab in ["vol", "skew", "varswap"] {
        assert!(
            overrides.contains(&format!("vol_views.{tab}")),
            "{overrides}"
        );
    }
}

/// A move that crosses only the user's own tabs forks nothing: no sidecar,
/// no copy notice, and the file holds the new order. The user layer holds
/// `mine` and its copy of the shipped `wing`, which the merge places
/// between `skew` and `wing`; `shift+k` on `wing` crosses `mine` alone.
#[gpui::test]
fn a_move_across_a_user_tab_forks_nothing(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let builtin = "config_version = 1\n[vol]\n[[vol.columns]]\nmeasure = \"atfm 3M\"\n[skew]\n[[skew.columns]]\nmeasure = \"skew 1Y\"\n[wing]\n[[wing.columns]]\nmeasure = \"wing skew 1Y\"\n";
    let user = "config_version = 1\n[mine]\n[[mine.columns]]\nkind = \"vega\"\n[wing]\n[[wing.columns]]\nmeasure = \"wing skew 3M\"\n";
    std::fs::write(dir.path().join("vol_views.toml"), user).unwrap();
    let mut services = services_over(builtin);
    let datasets = LayerDoc::builtin("datasets", SSR_DATASET).unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![LayerDoc::builtin("vol_views", builtin).unwrap(), datasets],
        desk: None,
        user: Some(dir.path().to_path_buf()),
    });
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::vol_views");
    let names = |shell: &Entity<ShellView>, cx: &gpui::VisualTestContext| {
        dialog_state(shell, cx, |s| {
            s.rows
                .rows()
                .iter()
                .map(|r| r.name.clone())
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(names(&shell, &cx), ["vol", "skew", "mine", "wing"]);
    cx.simulate_keystrokes("shift-g shift-k"); // wing up, across mine
    cx.run_until_parked();
    assert_eq!(names(&shell, &cx), ["vol", "skew", "wing", "mine"]);
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        notice.as_deref().is_none_or(|n| !n.starts_with("copied")),
        "{notice:?}"
    );
    flush_config_write(&mut cx);
    assert_eq!(names(&shell, &cx), ["vol", "skew", "wing", "mine"]);
    let text = std::fs::read_to_string(dir.path().join("vol_views.toml")).unwrap();
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle}: {text}"))
    };
    assert!(at("[wing]") < at("[mine]"), "{text}");
    assert!(
        !text.contains("[vol]") && !text.contains("[skew]"),
        "the shipped tabs the move did not cross stay shipped: {text}"
    );
    assert!(!dir.path().join("overrides.toml").exists());
}

/// Turning Default on for `skew` writes the top-level key alone — the
/// builtin tab is not copied — and the browse summary follows.
#[gpui::test]
fn the_default_bool_writes_the_top_level_key(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_vol_views(),
        dir.path(),
        "config::vol_views",
    );
    cx.simulate_keystrokes("j enter"); // skew
    cx.run_until_parked();
    // The cursor settles on `cells`; one down is Default.
    cx.simulate_keystrokes("j space");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| matches!(
        d.fields[2].kind,
        crate::shell::objectdialog::FieldKind::Bool(true)
    )));
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        notice.as_deref().is_none_or(|n| !n.starts_with("copied")),
        "the document's key is not a fork of the view: {notice:?}"
    );
    flush_config_write(&mut cx);
    let text = std::fs::read_to_string(dir.path().join("vol_views.toml")).unwrap();
    assert!(text.contains("default = \"skew\""), "{text}");
    assert!(!text.contains("[skew]"), "{text}");
    assert!(!dir.path().join("overrides.toml").exists());
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let summaries = dialog_state(&shell, &cx, |s| {
        s.rows
            .rows()
            .iter()
            .map(|r| r.summary.clone())
            .collect::<Vec<_>>()
    });
    assert!(summaries[1].ends_with(" · default"), "{summaries:?}");
    assert!(!summaries[0].ends_with(" · default"), "{summaries:?}");
}

/// Deleting the default tab clears the default key with it.
#[gpui::test]
fn deleting_the_default_tab_clears_the_default_key(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let user =
        "config_version = 1\ndefault = \"mine\"\n[mine]\n[[mine.columns]]\nkind = \"vega\"\n";
    std::fs::write(dir.path().join("vol_views.toml"), user).unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_user_vol_views(dir.path()),
        dir.path(),
        "config::vol_views",
    );
    cx.simulate_keystrokes("shift-g"); // mine, the last row
    cx.simulate_keystrokes("d y");
    cx.run_until_parked();
    flush_config_write(&mut cx);
    let text = std::fs::read_to_string(dir.path().join("vol_views.toml")).unwrap();
    assert!(!text.contains("default"), "{text}");
    assert!(!text.contains("[mine]"), "{text}");
    let names = dialog_state(&shell, &cx, |s| {
        s.rows
            .rows()
            .iter()
            .map(|r| r.name.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(names, ["vol", "skew"]);
}

/// [`services_with_vol_views`] over a "rec" roster whose tile answers
/// `config_object` with the `vol_views` tab `tab` — the stand-in for a vol
/// tile showing that tab.
fn services_with_vol_views_and_a_tile_showing(tab: &str) -> ShellServices {
    let rec = RecordingFactory::new("rec");
    *rec.config_object.borrow_mut() = Some(ConfigObject {
        doc: "vol_views",
        name: tab.to_string(),
    });
    let mut services = services_with_recorders(vec![rec]);
    let vol = services_with_vol_views();
    (services.config, services.builtin) = (vol.config, vol.builtin);
    services
}

/// A shell over `services` with one focused "rec" tile, then `action`
/// dispatched and a frame drawn — [`dialog_test_shell_with`] with a tile
/// under the dialog, which that helper never has.
fn dialog_test_shell_over_a_tile(
    cx: &mut gpui::TestAppContext,
    services: ShellServices,
    action: &str,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
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
    vcx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId(action.to_string()), None, window, cx);
        });
    });
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    (shell, vcx)
}

/// Opening over a vol tile lands the browse selection on the tab the tile shows.
#[gpui::test]
fn opening_from_a_vol_tile_selects_its_tab(cx: &mut gpui::TestAppContext) {
    let (shell, cx) = dialog_test_shell_over_a_tile(
        cx,
        services_with_vol_views_and_a_tile_showing("skew"),
        "config::vol_views",
    );
    let selected = dialog_state(&shell, &cx, |s| {
        s.rows.at(s.selected).map(|r| r.name.clone())
    });
    assert_eq!(selected.as_deref(), Some("skew"));
}

/// A tile naming a document this dialog does not edit changes nothing: the
/// first row stays selected, as with no tile at all.
#[gpui::test]
fn a_tile_showing_another_documents_object_leaves_the_first_row_selected(
    cx: &mut gpui::TestAppContext,
) {
    let rec = RecordingFactory::new("rec");
    *rec.config_object.borrow_mut() = Some(ConfigObject {
        doc: "views",
        name: "skew".to_string(),
    });
    let mut services = services_with_recorders(vec![rec]);
    let vol = services_with_vol_views();
    (services.config, services.builtin) = (vol.config, vol.builtin);
    let (shell, cx) = dialog_test_shell_over_a_tile(cx, services, "config::vol_views");
    let selected = dialog_state(&shell, &cx, |s| {
        s.rows.at(s.selected).map(|r| r.name.clone())
    });
    assert_eq!(selected.as_deref(), Some("vol"));
}

/// A history dataset keyed by underlying and tenor, as `SSR_DATASET` is,
/// under the name `add_user_dataset` substitutes for `{name}`.
const TWO_KEY_DATASET: &str = "[{name}]\nfamily = \"history\"\nkey = [\"underlying_ref\", \"tenor\"]\nhistory = \"800d\"\n[{name}.columns.underlying_ref]\ntype = \"utf8\"\nrole = \"dimension\"\n[{name}.columns.tenor]\ntype = \"utf8\"\nrole = \"dimension\"\n[{name}.columns.{name}]\ntype = \"f64\"\nrole = \"value\"\n";

/// Add `name` to the live config as a reload would: a user-layer
/// `datasets.toml` in the shell's user directory, loaded off the live
/// `services.builtin` and applied — the two steps the watcher schedules
/// (see `the_reload_an_edit_triggers…`). The startup baseline keeps the
/// datasets the shell launched with.
fn add_user_dataset(
    shell: &Entity<ShellView>,
    cx: &mut gpui::VisualTestContext,
    name: &str,
    template: &str,
) {
    let dir = shell
        .read_with(cx, |shell, cx| shell.user_dir(cx))
        .expect("a user directory");
    let text = format!("config_version = 1\n{}", template.replace("{name}", name));
    std::fs::write(dir.join("datasets.toml"), text).unwrap();
    let builtin = shell.read_with(cx, |shell, cx| shell.services(cx).builtin.clone());
    let reloaded = crate::reload::load_config(builtin, None, Some(dir));
    shell.update(cx, |shell, cx| {
        shell
            .app()
            .update(cx, |app, cx| app.apply_reload(reloaded, cx))
    });
    cx.run_until_parked();
}

/// A dataset added since launch is offered and warned about.
#[gpui::test]
fn a_dataset_added_since_launch_warns_that_it_needs_a_restart(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_vol_views(),
        dir.path(),
        "config::vol_views",
    );
    // Add a dataset to the live config as a reload would.
    add_user_dataset(&shell, &mut cx, "spotvol_cov", TWO_KEY_DATASET);
    cx.simulate_keystrokes("enter"); // vol
    cx.run_until_parked();
    // The catalogue's datasets sort by name: SPOTVOL_COV, then SSR last.
    cx.simulate_keystrokes("shift-g k space");
    cx.run_until_parked();
    let (items, diags) = edit_draft(&shell, &cx, |d| {
        (
            d.list_items("columns")
                .unwrap()
                .iter()
                .map(|i| i.name.clone())
                .collect::<Vec<_>>(),
            d.diagnostics
                .iter()
                .map(|d| (d.severity, d.message.clone(), d.path.clone()))
                .collect::<Vec<_>>(),
        )
    });
    assert_eq!(items, ["ATFM 3M", "Book vega", "SPOTVOL_COV 1W"]);
    assert!(
        diags.contains(&(
            Severity::Warning,
            "spotvol_cov needs a restart to paint".to_string(),
            Some("vol_views.vol.columns.2".to_string()),
        )),
        "{diags:?}"
    );
    // The dataset the shell launched with warns of nothing.
    cx.simulate_keystrokes("escape"); // back to browse, on vol
    cx.run_until_parked();
    cx.simulate_keystrokes("j enter"); // skew
    cx.run_until_parked();
    cx.simulate_keystrokes("shift-g space"); // SSR
    cx.run_until_parked();
    let (items, diags) = edit_draft(&shell, &cx, |d| {
        (
            d.list_items("columns")
                .unwrap()
                .iter()
                .map(|i| i.name.clone())
                .collect::<Vec<_>>(),
            d.diagnostics
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>(),
        )
    });
    assert_eq!(items, ["25Δ skew 1Y · slider", "SSR 1W"]);
    assert!(
        !diags.iter().any(|m| m.contains("needs a restart")),
        "{diags:?}"
    );
}

/// A failed write's revert rebuilds the draft from the reverted config; a
/// restart warning a saved column carries survives the rebuild.
#[gpui::test]
fn the_restart_warning_survives_a_failed_writes_revert(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_vol_views(),
        dir.path(),
        "config::vol_views",
    );
    add_user_dataset(&shell, &mut cx, "spotvol_cov", TWO_KEY_DATASET);
    cx.simulate_keystrokes("enter"); // vol
    cx.run_until_parked();
    cx.simulate_keystrokes("shift-g k space"); // SPOTVOL_COV
    cx.run_until_parked();
    flush_config_write(&mut cx); // saved: the column is in the live config
    let text = std::fs::read_to_string(dir.path().join("vol_views.toml")).unwrap();
    assert!(text.contains("measure = \"spotvol_cov 1W\""), "{text}");
    // Unparseable and never loaded by this shell, so only the next write
    // can discover it (`config_write::edit` refuses the file untouched).
    std::fs::write(dir.path().join("vol_views.toml"), "[vol\nmeasure =").unwrap();
    cx.simulate_keystrokes("shift-g space"); // SSR
    cx.run_until_parked();
    flush_config_write(&mut cx); // fails; the revert rebuilds the draft
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        notice.as_deref().is_some_and(|n| n.contains("reverted")),
        "{notice:?}"
    );
    let (items, diags) = edit_draft(&shell, &cx, |d| {
        (
            d.list_items("columns")
                .unwrap()
                .iter()
                .map(|i| i.name.clone())
                .collect::<Vec<_>>(),
            d.diagnostics
                .iter()
                .map(|d| (d.severity, d.message.clone(), d.path.clone()))
                .collect::<Vec<_>>(),
        )
    });
    assert_eq!(
        items,
        ["ATFM 3M", "Book vega", "SPOTVOL_COV 1W"],
        "the refused SSR column is gone, the saved one stays"
    );
    assert!(
        diags.contains(&(
            Severity::Warning,
            "spotvol_cov needs a restart to paint".to_string(),
            Some("vol_views.vol.columns.2".to_string()),
        )),
        "the rebuilt draft must carry the warning: {diags:?}"
    );
}
