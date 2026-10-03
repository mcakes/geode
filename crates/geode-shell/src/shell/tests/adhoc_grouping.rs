//! The lane's ad hoc grouping outside the dialog: session restore, reload
//! retention, and the `frame::grouping_adhoc` action.

use super::*;
use crate::frame::GroupingChoice;

/// `book` and `lhu` as groupable dimensions. The position-grain measure
/// declares the grain that carries them: `groupable_columns` offers nothing
/// from a dataset with no grain to scan.
const DATASETS: &str = "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
    [risk.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\n\
    [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
    [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n";

/// The same dataset without `lhu`.
const DATASETS_WITHOUT_LHU: &str = "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
    [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
    [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n";

fn sources(datasets: &str) -> ConfigSources {
    ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            LayerDoc::builtin("groupings", "1 = [\"book\"]\n").unwrap(),
            LayerDoc::builtin("datasets", datasets).unwrap(),
        ],
        ..ConfigSources::default()
    }
}

fn services() -> ShellServices {
    let mut services = test_services();
    (services.config, services.builtin) = ShellServices::config_and_builtin(sources(DATASETS));
    services
}

fn record(slot: Option<u8>, ad_hoc: &[&str], active: bool) -> crate::session::FrameRecord {
    crate::session::FrameRecord {
        scope: geode_core::scope::Scope::default(),
        active_slot: slot,
        ad_hoc: (!ad_hoc.is_empty()).then(|| ad_hoc.iter().map(|c| c.to_string()).collect()),
        ad_hoc_active: active,
        as_of: geode_core::query::AsOf::Live,
    }
}

fn chain(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| n.to_string()).collect()
}

#[gpui::test]
fn a_restored_active_ad_hoc_chain_is_the_grouping_in_force(cx: &mut gpui::TestAppContext) {
    let mut services = services();
    services.restored_frame = Some(record(None, &["lhu", "book"], true));
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().grouping_choice()),
        GroupingChoice::AdHoc
    );
    assert_eq!(
        frame.read_with(&vcx, |f, _| f
            .shared()
            .active_grouping()
            .map(<[String]>::to_vec)),
        Some(chain(&["lhu", "book"]))
    );
}

#[gpui::test]
fn a_restored_inactive_chain_is_kept_beside_the_slot(cx: &mut gpui::TestAppContext) {
    let mut services = services();
    services.restored_frame = Some(record(Some(1), &["lhu"], false));
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().grouping_choice()),
        GroupingChoice::Slot(1)
    );
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().ad_hoc().map(<[String]>::to_vec)),
        Some(chain(&["lhu"]))
    );
}

#[gpui::test]
fn a_restored_chain_naming_an_unknown_column_is_dropped(cx: &mut gpui::TestAppContext) {
    let mut services = services();
    services.restored_frame = Some(record(None, &["book", "nope"], true));
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().ad_hoc().map(<[String]>::to_vec)),
        None,
        "dropped whole, never narrowed to [book]"
    );
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().grouping_choice()),
        GroupingChoice::ViewDefault
    );
}

#[gpui::test]
fn a_pinned_lane_restores_its_own_ad_hoc_chain(cx: &mut gpui::TestAppContext) {
    let mut services = services();
    let ws1 = crate::tiling::WorkspaceIx::new(1).unwrap();
    services.restored_frame = Some(record(Some(1), &[], false));
    services
        .restored_pinned
        .insert(ws1, record(None, &["lhu"], true));
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.view(ws1).grouping_choice()),
        GroupingChoice::AdHoc
    );
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().grouping_choice()),
        GroupingChoice::Slot(1),
        "the shared lane keeps its own choice"
    );
}

#[gpui::test]
fn a_pinned_lane_without_a_chain_does_not_inherit_the_shared_one(cx: &mut gpui::TestAppContext) {
    let mut services = services();
    let ws1 = crate::tiling::WorkspaceIx::new(1).unwrap();
    services.restored_frame = Some(record(None, &["lhu", "book"], true));
    services
        .restored_pinned
        .insert(ws1, record(Some(1), &[], false));
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.view(ws1).ad_hoc().map(<[String]>::to_vec)),
        None,
        "pinning copied the shared chain; the pinned record has none"
    );
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.view(ws1).grouping_choice()),
        GroupingChoice::Slot(1)
    );
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().ad_hoc().map(<[String]>::to_vec)),
        Some(chain(&["lhu", "book"])),
        "the shared lane keeps its own chain"
    );
}

#[gpui::test]
fn a_reload_that_removes_a_column_drops_the_chain_naming_it(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        assert!(f.shared_mut().set_ad_hoc(chain(&["lhu", "book"])));
        cx.notify();
    });
    vcx.run_until_parked();
    let before = frame.read_with(&vcx, |f, _| f.shared().versions());

    let (config, _) = ShellServices::config_and_builtin(sources(DATASETS_WITHOUT_LHU));
    shell.update(&mut vcx, |s, cx| s.apply_reload(config, cx));
    vcx.run_until_parked();

    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().ad_hoc().map(<[String]>::to_vec)),
        None
    );
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().grouping_choice()),
        GroupingChoice::ViewDefault
    );
    assert_ne!(
        frame.read_with(&vcx, |f, _| f.shared().versions().grouping),
        before.grouping,
        "tiles following the dropped chain must requery"
    );
}

/// The row menu's `Color…` names come from the current datasets: built at
/// construction, rebuilt when a reload changes them.
#[gpui::test]
fn a_reload_that_removes_a_column_rebuilds_the_text_dimensions(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services());
    let shell = shell_of(&window, &mut vcx);
    let names = |shell: &Entity<ShellView>, vcx: &gpui::VisualTestContext| {
        shell.read_with(vcx, |s, _| s.text_dims.iter().cloned().collect::<Vec<_>>())
    };
    assert_eq!(names(&shell, &vcx), chain(&["book", "lhu", "position_ref"]));

    let (config, _) = ShellServices::config_and_builtin(sources(DATASETS_WITHOUT_LHU));
    shell.update(&mut vcx, |s, cx| s.apply_reload(config, cx));
    vcx.run_until_parked();

    assert_eq!(names(&shell, &vcx), chain(&["book", "position_ref"]));
}

#[gpui::test]
fn a_reload_that_keeps_every_column_keeps_the_chain(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        f.shared_mut().set_ad_hoc(chain(&["book"]));
        cx.notify();
    });
    let (config, _) = ShellServices::config_and_builtin(sources(DATASETS_WITHOUT_LHU));
    shell.update(&mut vcx, |s, cx| s.apply_reload(config, cx));
    vcx.run_until_parked();
    assert_eq!(
        frame.read_with(&vcx, |f, _| f
            .shared()
            .active_grouping()
            .map(<[String]>::to_vec)),
        Some(chain(&["book"]))
    );
}

#[gpui::test]
fn the_ad_hoc_action_returns_to_the_stored_chain(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        f.shared_mut().set_ad_hoc(chain(&["lhu", "book"]));
        f.shared_mut().set_active_slot(Some(1));
        cx.notify();
    });
    vcx.run_until_parked();

    dispatch_action(&shell, "frame::grouping_adhoc", &mut vcx);
    vcx.run_until_parked();

    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().grouping_choice()),
        GroupingChoice::AdHoc
    );
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
}

#[gpui::test]
fn the_ad_hoc_action_with_nothing_stored_says_so(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    let before = frame.read_with(&vcx, |f, _| f.shared().versions());

    dispatch_action(&shell, "frame::grouping_adhoc", &mut vcx);
    vcx.run_until_parked();

    assert_eq!(
        shell.read_with(&vcx, |s, _| s.notice.clone()).as_deref(),
        Some(crate::shell::input::NO_AD_HOC)
    );
    assert_eq!(frame.read_with(&vcx, |f, _| f.shared().versions()), before);
    assert!(
        shell.read_with(&vcx, |s, _| !s.modal_open()),
        "the action never opens a dialog, so it stays out of opens_dialog"
    );
}

#[gpui::test]
fn the_ad_hoc_action_is_in_the_palette_under_frame(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services());
    let shell = shell_of(&window, &mut vcx);
    let title = shell.read_with(&vcx, |s, _| {
        s.services
            .registry
            .iter()
            .find(|d| d.id.0 == "frame::grouping_adhoc")
            .map(|d| (d.title.clone(), d.category.clone()))
    });
    assert_eq!(
        title,
        Some(("Ad hoc grouping".to_string(), "Frame".to_string()))
    );
}

/// One frame table from a written session: `[frame]` for `None`, else
/// `workspaces.N.frame`.
fn written_frame(text: &str, ws: Option<u8>) -> Option<toml::Table> {
    let root: toml::Table = text.parse().unwrap();
    let table = match ws {
        None => root.get("frame"),
        Some(n) => root
            .get("workspaces")
            .and_then(|w| w.get(n.to_string()))
            .and_then(|w| w.get("frame")),
    };
    table.and_then(|t| t.as_table()).cloned()
}

fn written_chain(table: &toml::Table) -> Option<Vec<String>> {
    table.get("ad_hoc").and_then(|v| v.as_array()).map(|items| {
        items
            .iter()
            .map(|i| i.as_str().unwrap().to_string())
            .collect()
    })
}

/// The session writer captures each lane's chain and whether it is active:
/// without it a chain the user built would vanish on restart. Forgetting
/// an inactive chain must reach the next write too, though nothing requeries.
#[gpui::test]
fn the_session_writer_saves_each_lanes_ad_hoc_chain(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let mut services = services();
    services.session_path = Some(dir.path().join("session.toml"));
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    let ws2 = crate::tiling::WorkspaceIx::new(2).unwrap();
    dispatch_action(&shell, "workspace::switch_2", &mut vcx);
    vcx.run_until_parked();
    // Baseline: whatever startup left dirty is written first.
    let _ = shell.update(&mut vcx, |s, cx| s.take_dirty_session_write(cx));

    frame.update(&mut vcx, |f, cx| {
        assert!(f.shared_mut().set_ad_hoc(chain(&["lhu", "book"])));
        assert!(f.pin(ws2));
        assert!(f.view_mut(ws2).set_ad_hoc(chain(&["book"])));
        cx.notify();
    });
    vcx.run_until_parked();

    let (_, text) = shell
        .update(&mut vcx, |s, cx| s.take_dirty_session_write(cx))
        .expect("an ad hoc chain and a pin dirty the session");
    let shared = written_frame(&text, None).expect("[frame] is written");
    assert_eq!(written_chain(&shared), Some(chain(&["lhu", "book"])));
    assert_eq!(
        shared.get("grouping").and_then(|v| v.as_str()),
        Some("ad_hoc")
    );
    assert!(!shared.contains_key("slot"), "{shared:?}");
    let pinned = written_frame(&text, Some(2)).expect("workspaces.2.frame is written");
    assert_eq!(
        written_chain(&pinned),
        Some(chain(&["book"])),
        "the pinned lane writes its own chain"
    );
    assert_eq!(
        pinned.get("grouping").and_then(|v| v.as_str()),
        Some("ad_hoc")
    );
    assert!(!pinned.contains_key("slot"), "{pinned:?}");

    frame.update(&mut vcx, |f, cx| {
        assert!(f.shared_mut().set_active_slot(Some(1)));
        cx.notify();
    });
    vcx.run_until_parked();
    let _ = shell.update(&mut vcx, |s, cx| s.take_dirty_session_write(cx));
    frame.update(&mut vcx, |f, cx| {
        assert!(f.shared_mut().forget_ad_hoc());
        cx.notify();
    });
    vcx.run_until_parked();
    let (_, text) = shell
        .update(&mut vcx, |s, cx| s.take_dirty_session_write(cx))
        .expect("forgetting an inactive chain dirties the session");
    let shared = written_frame(&text, None).expect("[frame] is written");
    assert!(!shared.contains_key("ad_hoc"), "{shared:?}");
    assert!(!shared.contains_key("grouping"), "{shared:?}");
    assert_eq!(shared.get("slot").and_then(|v| v.as_integer()), Some(1));
}
