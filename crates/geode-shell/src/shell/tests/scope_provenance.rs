//! The lane's scope provenance across a session: written from the frame and
//! restored into it only while the named saved scope still exists.

use super::*;

const DATASETS: &str = "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
    [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n";

fn services() -> ShellServices {
    let mut services = test_services();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            LayerDoc::builtin("datasets", DATASETS).unwrap(),
            LayerDoc::builtin("scopes", "[eu]\n[eu.dimensions]\nbook = [\"BK001\"]\n").unwrap(),
        ],
        ..ConfigSources::default()
    });
    services
}

fn record(loaded_from: Option<&str>) -> crate::session::FrameRecord {
    crate::session::FrameRecord {
        scope: geode_core::scope::Scope::one("book", "BK001"),
        active_slot: None,
        ad_hoc: None,
        ad_hoc_active: false,
        loaded_from: loaded_from.map(str::to_string),
        as_of: geode_core::query::AsOf::Live,
    }
}

fn shared_source(frame: &Entity<Frame>, vcx: &gpui::VisualTestContext) -> Option<String> {
    frame.read_with(vcx, |f, _| f.shared().loaded_from().map(str::to_string))
}

#[gpui::test]
fn a_restored_provenance_names_the_saved_scope(cx: &mut gpui::TestAppContext) {
    let mut services = services();
    services.restored_frame = Some(record(Some("eu")));
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert_eq!(shared_source(&frame, &vcx), Some("eu".to_string()));
}

#[gpui::test]
fn a_restored_provenance_naming_no_saved_scope_is_dropped(cx: &mut gpui::TestAppContext) {
    let mut services = services();
    services.restored_frame = Some(record(Some("gone")));
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert_eq!(shared_source(&frame, &vcx), None);
}

#[gpui::test]
fn a_pinned_lanes_provenance_is_restored_into_that_lane(cx: &mut gpui::TestAppContext) {
    let mut services = services();
    let ws = crate::tiling::WorkspaceIx::new(1).unwrap();
    services.restored_pinned.insert(ws, record(Some("eu")));
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.view(ws).loaded_from().map(str::to_string)),
        Some("eu".to_string())
    );
    assert_eq!(
        shared_source(&frame, &vcx),
        None,
        "the pinned record names only its own lane"
    );
}

/// `pin` copies the shared lane's provenance; a pinned record written
/// without one must not keep that copy.
#[gpui::test]
fn a_pinned_record_without_provenance_does_not_inherit_the_shared_lanes(
    cx: &mut gpui::TestAppContext,
) {
    let mut services = services();
    let ws = crate::tiling::WorkspaceIx::new(1).unwrap();
    services.restored_frame = Some(record(Some("eu")));
    services.restored_pinned.insert(ws, record(None));
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert_eq!(shared_source(&frame, &vcx), Some("eu".to_string()));
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.view(ws).loaded_from().map(str::to_string)),
        None
    );
}

#[gpui::test]
fn the_session_snapshot_carries_the_lanes_provenance(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let mut services = services();
    services.session_path = Some(dir.path().join("session.toml"));
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    // Baseline: whatever startup left dirty is written first.
    let _ = shell.update(&mut vcx, |s, cx| s.take_dirty_session_write(cx));

    frame.update(&mut vcx, |f, cx| {
        f.shared_mut().load_scope("eu").unwrap();
        cx.notify();
    });
    vcx.run_until_parked();

    let (_, text) = shell
        .update(&mut vcx, |s, cx| s.take_dirty_session_write(cx))
        .expect("a load dirties the session");
    assert!(text.contains("loaded_from = \"eu\""), "{text}");
}
