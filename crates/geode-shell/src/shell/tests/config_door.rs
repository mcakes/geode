//! The module config door: whole objects a tile queues on its frame reach
//! the user layer through the object dialogs' pending batch.

use super::*;
use crate::frame::{ConfigEdit, TileNotice};
use crate::tiling::TileId;
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

/// Queue edits the way a module does: through a frame handle, which stamps
/// the handle's tile as the origin and wakes the shell's drain.
fn queue(shell: &Entity<ShellView>, edit: ConfigEdit, cx: &mut gpui::VisualTestContext) {
    queue_from(shell, None, vec![edit], cx);
}

fn queue_from(
    shell: &Entity<ShellView>,
    tile: Option<TileId>,
    edits: Vec<ConfigEdit>,
    cx: &mut gpui::VisualTestContext,
) {
    let frame = shell.read_with(cx, |s, _| s.active_frame());
    let handle = match tile {
        Some(tile) => FrameRef::for_tile(frame.entity().clone(), frame.workspace(), tile),
        None => frame,
    };
    cx.update(|_, cx| handle.queue_config_edits(edits, cx));
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
                origin: None,
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
            origin: None,
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
            origin: None,
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

/// The tile every fork test queues from.
const TILE: TileId = TileId(7);

/// [`services_with_book`] plus a desk-layer `dimensions` document defining
/// `desk`, so a user-layer write of `desk` shadows the desk's copy. The desk
/// document travels in `builtin` with `Layer::Desk`, as the object dialog's
/// desk fixtures layer theirs.
fn services_with_a_desk_classification() -> ShellServices {
    let mut services = test_services();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin(
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
            )
            .unwrap(),
            LayerDoc {
                layer: Layer::Desk,
                name: geode_core::config::DIMENSIONS_DOC.to_string(),
                file: "<test:desk>".into(),
                table: "[desk]\nfrom = \"book\"\n[desk.values]\nDesk = [\"BK000\"]\n"
                    .parse()
                    .unwrap(),
            },
        ],
        desk: None,
        user: None,
    });
    services
}

/// The sidecar entry the desk's copy of `desk` must be recorded as.
fn desk_baseline() -> toml::Value {
    let mut values = toml::Table::new();
    values.insert("Desk".into(), toml::Value::Array(vec!["BK000".into()]));
    let mut table = toml::Table::new();
    table.insert("from".into(), "book".into());
    table.insert("values".into(), toml::Value::Table(values));
    crate::shell::objectdialog::override_entry(Layer::Desk, "desk", &toml::Value::Table(table))
}

/// A whole `desk` classification with one label.
fn desk_edit(label: &str) -> ConfigEdit {
    ConfigEdit {
        doc: geode_core::config::DIMENSIONS_DOC,
        object: "desk".into(),
        value: Some(sector(label)),
        origin: None,
    }
}

/// Past the debounce, with every write and reload landed.
fn flush(cx: &mut gpui::VisualTestContext) {
    cx.run_until_parked();
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(300));
    cx.run_until_parked();
}

/// The user-layer sidecar entry recorded for `dimensions.desk`, if any.
fn sidecar_entry(dir: &std::path::Path) -> Option<toml::Value> {
    let text = std::fs::read_to_string(dir.join("overrides.toml")).ok()?;
    let mut table: toml::Table = text.parse().unwrap();
    table.remove("dimensions.desk")
}

fn take_notices(shell: &Entity<ShellView>, cx: &mut gpui::VisualTestContext) -> Vec<TileNotice> {
    let frame = shell.read_with(cx, |s, _| s.active_frame());
    cx.update(|_, cx| frame.update(cx, |f, _| f.take_tile_notices(TILE)))
}

/// A tile's edit to an object a lower layer defines forks it like a dialog
/// edit: the overrides sidecar records the inherited value in the same
/// batch, and the origin tile is told once.
#[gpui::test]
fn a_door_edit_to_a_desk_object_forks_records_and_tells_its_tile(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (window, mut cx) =
        open_shell_with_user_dir(cx, services_with_a_desk_classification(), dir.path());
    let shell = shell_of(&window, &mut cx);
    queue_from(&shell, Some(TILE), vec![desk_edit("Mine")], &mut cx);
    flush(&mut cx);

    let text = std::fs::read_to_string(dir.path().join("dimensions.toml")).unwrap();
    assert!(text.contains("[desk") && text.contains("Mine"), "{text}");
    assert_eq!(
        sidecar_entry(dir.path()),
        Some(desk_baseline()),
        "the sidecar records the desk's copy as the fork's baseline"
    );
    assert_eq!(
        take_notices(&shell, &mut cx),
        vec![TileNotice::Forked(
            "copied 'desk' to your config — Revert… restores the desk copy".to_string()
        )]
    );
    assert_eq!(take_notices(&shell, &mut cx), vec![], "a take drains");
}

/// A second edit to the now user-owned object does not fork again: its
/// baseline in the sidecar must not be overwritten.
#[gpui::test]
fn a_second_door_edit_does_not_fork_again(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (window, mut cx) =
        open_shell_with_user_dir(cx, services_with_a_desk_classification(), dir.path());
    let shell = shell_of(&window, &mut cx);
    queue_from(&shell, Some(TILE), vec![desk_edit("First")], &mut cx);
    flush(&mut cx);
    queue_from(&shell, Some(TILE), vec![desk_edit("Second")], &mut cx);
    flush(&mut cx);

    let text = std::fs::read_to_string(dir.path().join("dimensions.toml")).unwrap();
    assert!(text.contains("Second") && !text.contains("First"), "{text}");
    assert_eq!(
        sidecar_entry(dir.path()),
        Some(desk_baseline()),
        "the baseline stays the desk's copy, not the first edit"
    );
    assert_eq!(
        take_notices(&shell, &mut cx).len(),
        1,
        "one fork, one notice"
    );
}

/// Two edits inside one debounce: the second sees the first through the
/// pending batch, so it neither forks again nor overwrites the baseline.
#[gpui::test]
fn two_door_edits_in_one_debounce_fork_once(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (window, mut cx) =
        open_shell_with_user_dir(cx, services_with_a_desk_classification(), dir.path());
    let shell = shell_of(&window, &mut cx);
    queue_from(&shell, Some(TILE), vec![desk_edit("First")], &mut cx);
    cx.run_until_parked();
    queue_from(&shell, Some(TILE), vec![desk_edit("Second")], &mut cx);
    flush(&mut cx);

    assert_eq!(sidecar_entry(dir.path()), Some(desk_baseline()));
    assert_eq!(
        take_notices(&shell, &mut cx).len(),
        1,
        "one fork, one notice"
    );
}

/// Two edits to one inherited object in a single drain: the second finds
/// the first already the user's, so the tile hears of one fork.
#[gpui::test]
fn two_door_edits_in_one_drain_fork_once(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (window, mut cx) =
        open_shell_with_user_dir(cx, services_with_a_desk_classification(), dir.path());
    let shell = shell_of(&window, &mut cx);
    queue_from(
        &shell,
        Some(TILE),
        vec![desk_edit("First"), desk_edit("Second")],
        &mut cx,
    );
    flush(&mut cx);

    assert_eq!(sidecar_entry(dir.path()), Some(desk_baseline()));
    assert_eq!(
        take_notices(&shell, &mut cx).len(),
        1,
        "one fork, one notice"
    );
}

/// With no writable user directory the origin tile hears the refusal, once
/// per drain however many of its edits were refused.
#[gpui::test]
fn a_refused_door_edit_tells_its_tile(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, services_with_a_desk_classification());
    let shell = shell_of(&window, &mut cx);
    queue_from(
        &shell,
        Some(TILE),
        vec![desk_edit("Mine"), desk_edit("Again")],
        &mut cx,
    );
    cx.run_until_parked();
    assert_eq!(
        take_notices(&shell, &mut cx),
        vec![TileNotice::Refused(
            "no writable user config directory — nothing was changed".to_string()
        )]
    );
}
