//! The Calibration stages object dialog through the shell: `config::calibration`
//! lands on the focused tile's stage at its Edit screen (and on Browse
//! without one), a tile's requested action reaches the shell only from the
//! focused tile, and a stage edit is written under the `stages` table.

use super::objectdialog::{dialog_state, edit_draft, flush_config_write};
use super::volviews_dialog::dialog_test_shell_over_a_tile;
use super::*;
use crate::defaults::AddPlacement;
use crate::module::ConfigObject;
use crate::module::recording::RecordingFactory;
use crate::shell::objectdialog::{Domain, Stage};
use geode_core::calibration::{BUILTIN_CALIBRATION, CALIBRATION_DOC};

/// The demo schema: the datasets the builtin pipeline's sources read and
/// its publish stage writes are document datasets here.
const DEMO_DATASETS: &str = include_str!("../../../../../examples/demo-config/datasets.toml");
/// The demo `sophis` target, so the builtin `publish` stage's target is a
/// choice the dialog offers.
const EGRESS: &str = "config_version = 1\n[sophis]\nadapter = \"demo_bus\"\n[sophis.documents]\nrate_curve = \"marketdata/ratecurve/{key}/NOTIFY\"\n";

/// Services over the builtin `calibration.toml`, the demo schema and one
/// egress target, with `rec` as the one tile kind.
fn calibration_services(rec: RecordingFactory) -> ShellServices {
    let mut services = services_with_recorders(vec![rec]);
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin(CALIBRATION_DOC, BUILTIN_CALIBRATION).unwrap(),
            LayerDoc::builtin("datasets", DEMO_DATASETS).unwrap(),
            LayerDoc::builtin("egress", EGRESS).unwrap(),
        ],
        desk: None,
        user: None,
    });
    services
}

/// A recorder whose tiles answer `config_object` with `object`.
fn recorder_showing(object: Option<ConfigObject>) -> RecordingFactory {
    let rec = RecordingFactory::new("rec");
    *rec.config_object.borrow_mut() = object;
    rec
}

fn stage(name: &str) -> Option<ConfigObject> {
    Some(ConfigObject {
        doc: CALIBRATION_DOC,
        name: name.to_string(),
    })
}

/// Over a tile showing `repo_fit`, `config::calibration` opens straight on
/// that stage's Edit screen with its kind's fields; over a tile showing no
/// configuration object, or a stage the document lacks, it opens on Browse.
#[gpui::test]
fn config_calibration_opens_the_focused_tiles_stage_in_edit(cx: &mut gpui::TestAppContext) {
    let (shell, cx) = dialog_test_shell_over_a_tile(
        cx,
        calibration_services(recorder_showing(stage("repo_fit"))),
        "config::calibration",
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| (s.domain, s.stage.clone())),
        (
            Domain::Calibration,
            Stage::Edit {
                object: "repo_fit".into()
            }
        )
    );
    let keys = edit_draft(&shell, &cx, |d| {
        d.fields.iter().map(|f| f.key.clone()).collect::<Vec<_>>()
    });
    assert_eq!(
        keys,
        [
            "pipeline",
            "kind",
            "inputs",
            "series",
            "knots",
            "process_noise",
            "measurement_noise"
        ]
    );
}

#[gpui::test]
fn config_calibration_over_a_tile_showing_nothing_opens_on_browse(cx: &mut gpui::TestAppContext) {
    let (shell, cx) = dialog_test_shell_over_a_tile(
        cx,
        calibration_services(recorder_showing(None)),
        "config::calibration",
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        Stage::Browse
    );
    let first = dialog_state(&shell, &cx, |s| s.rows.at(0).map(|r| r.name.clone()));
    assert_eq!(
        first.as_deref(),
        Some("chain"),
        "document order, the sources first"
    );
}

#[gpui::test]
fn config_calibration_over_a_tile_showing_an_unknown_stage_opens_on_browse(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, cx) = dialog_test_shell_over_a_tile(
        cx,
        calibration_services(recorder_showing(stage("nope"))),
        "config::calibration",
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        Stage::Browse
    );
}

/// The tile→shell action seam: a `Frame::request_action` from the focused
/// tile runs the action as a key would (here it opens the dialog); one from
/// a tile that is not focused is dropped.
#[gpui::test]
fn a_requested_action_opens_the_dialog_only_from_the_focused_tile(cx: &mut gpui::TestAppContext) {
    let rec = recorder_showing(None);
    let handles = rec.frame_handles.clone();
    let (window, mut vcx) = open_shell(cx, calibration_services(rec));
    let shell = shell_of(&window, &mut vcx);
    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.add_tile("rec", AddPlacement::Split(None), None, window, cx);
            s.add_tile("rec", AddPlacement::Split(None), None, window, cx);
        })
    });
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.run_until_parked();
    let focused = shell
        .read_with(&vcx, |s, cx| s.workspace(cx).focused_tile())
        .expect("the second tile is focused");
    let other = handles
        .borrow()
        .keys()
        .copied()
        .find(|t| *t != focused)
        .expect("two tiles were created");
    let action = || ActionId("config::calibration".to_string());

    let unfocused = handles.borrow()[&other].clone();
    vcx.update(|_, cx| unfocused.request_action(action(), cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        shell.read_with(&vcx, |s, _| s.object_dialog.is_none()),
        "a request from a tile that is not focused is dropped"
    );

    let focused_handle = handles.borrow()[&focused].clone();
    vcx.update(|_, cx| focused_handle.request_action(action(), cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        dialog_state(&shell, &vcx, |s| (s.domain, s.stage.clone())),
        (Domain::Calibration, Stage::Browse)
    );
}

/// A typed `series` on the builtin `chain` stage is written as
/// `[stages.chain]` in the user file, not as a top-level `[chain]`, and
/// the fork of the shipped stage is announced; memory holds the same
/// object under the same table.
#[gpui::test]
fn a_stage_edit_is_written_under_the_stages_table(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        calibration_services(recorder_showing(None)),
        dir.path(),
        "config::calibration",
    );
    cx.simulate_keystrokes("enter"); // chain, the first row
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        Stage::Edit { object } if object == "chain"
    ));
    cx.simulate_keystrokes("shift-g"); // the last row: a source's `series`
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    cx.simulate_input("funding");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.text("series").map(str::to_string)).as_deref(),
        Some("funding")
    );
    flush_config_write(&mut cx);
    let text = std::fs::read_to_string(dir.path().join("calibration.toml")).unwrap();
    assert!(text.contains("[stages.chain]"), "{text}");
    assert!(text.contains("series = \"funding\""), "{text}");
    assert!(
        !text.contains("\n[chain]"),
        "a stage is never a top-level object: {text}"
    );
    let in_memory = shell.read_with(&cx, |s, cx| {
        s.services(cx)
            .config
            .doc(CALIBRATION_DOC)
            .and_then(|d| d.value.get("stages")?.get("chain")?.get("series")?.as_str())
            .map(str::to_string)
    });
    assert_eq!(in_memory.as_deref(), Some("funding"));
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone());
    assert!(
        notice
            .as_deref()
            .is_some_and(|n| n.starts_with("copied 'chain' to your config")),
        "{notice:?}"
    );
}
