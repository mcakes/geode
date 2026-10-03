//! The module config door: whole objects a tile queues on its frame reach
//! the user layer through the object dialogs' pending batch.

use super::*;
use crate::frame::ConfigEdit;
use geode_core::dimensions::DerivedDimensions;

/// Shared fixture services whose builtin `datasets` declares the utf8
/// dimension `book`, so a classification `from = "book"` is accepted by the
/// in-memory reload rather than kept-last-good.
fn services_with_book() -> ShellServices {
    let mut services = test_services();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin(
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
            )
            .unwrap(),
        ],
        ..ConfigSources::default()
    });
    services
}

fn sector(label: &str) -> toml::Value {
    let mut values = toml::Table::new();
    values.insert(label.to_string(), toml::Value::Array(vec!["BK000".into()]));
    let mut table = toml::Table::new();
    table.insert("from".into(), "book".into());
    table.insert("values".into(), toml::Value::Table(values));
    toml::Value::Table(table)
}

fn queue(shell: &Entity<ShellView>, edit: ConfigEdit, cx: &mut gpui::VisualTestContext) {
    let frame = shell.read_with(cx, |s, _| s.active_frame());
    cx.update(|_, cx| {
        frame.update(cx, |f, cx| {
            f.queue_config_edits(vec![edit]);
            cx.notify();
        })
    });
}

/// A module's queued classification lands in the user layer's
/// `dimensions.toml` through the shared batch, and a burst of edits within
/// the debounce coalesces into one write carrying the last value.
#[gpui::test]
fn queued_config_edits_reach_the_user_layer_through_the_batch(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (window, mut cx) = open_shell_with_user_dir(cx, services_with_book(), dir.path());
    let shell = shell_of(&window, &mut cx);
    for label in ["Tech", "Index"] {
        queue(
            &shell,
            ConfigEdit {
                doc: geode_core::config::DIMENSIONS_DOC,
                object: "sector".into(),
                value: Some(sector(label)),
            },
            &mut cx,
        );
    }
    // Inside the debounce nothing has been written yet.
    cx.run_until_parked();
    assert!(!dir.path().join("dimensions.toml").exists());

    cx.executor()
        .advance_clock(std::time::Duration::from_millis(300));
    cx.run_until_parked();
    let text = std::fs::read_to_string(dir.path().join("dimensions.toml")).unwrap();
    assert!(text.contains("Index") && !text.contains("Tech"), "{text}");
    // Memory took the edit too: the reloaded config knows the dimension.
    let known = shell.read_with(&cx, |s, _| {
        s.services
            .config
            .doc("dimensions")
            .map(|d| DerivedDimensions::from_doc(d).0.get("sector").is_some())
            .unwrap_or(false)
    });
    assert!(known, "the in-memory reload must accept the classification");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.config_write_error.clone()),
        None
    );
}

#[gpui::test]
fn a_queued_removal_deletes_the_user_object(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dimensions.toml"),
        "config_version = 1\n[sector]\nfrom = \"book\"\n[sector.values]\nTech = [\"BK000\"]\n",
    )
    .unwrap();
    let (window, mut cx) = open_shell_with_user_dir(cx, services_with_book(), dir.path());
    let shell = shell_of(&window, &mut cx);
    queue(
        &shell,
        ConfigEdit {
            doc: geode_core::config::DIMENSIONS_DOC,
            object: "sector".into(),
            value: None,
        },
        &mut cx,
    );
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(300));
    cx.run_until_parked();
    let text = std::fs::read_to_string(dir.path().join("dimensions.toml")).unwrap();
    assert!(!text.contains("sector"), "{text}");
}

/// Without a writable user directory the drain refuses visibly: the status
/// carries the refusal and nothing is pending.
#[gpui::test]
fn a_queued_edit_without_a_user_dir_reports_the_refusal(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, services_with_book());
    let shell = shell_of(&window, &mut cx);
    queue(
        &shell,
        ConfigEdit {
            doc: geode_core::config::DIMENSIONS_DOC,
            object: "sector".into(),
            value: Some(sector("Tech")),
        },
        &mut cx,
    );
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |s, _| s.config_write_error.clone()),
        Some("no writable user config directory — nothing was changed".into())
    );
    assert!(shell.read_with(&cx, |s, _| s.pending_config_write.is_none()));
}
