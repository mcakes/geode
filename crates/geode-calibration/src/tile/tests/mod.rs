//! The tile's window tests: `harness` hosts it as the shell does, with
//! keys resolved through the app's keymap and answers through the
//! content's `deliver`.

mod data;
mod edits;
mod harness;
mod keys;
mod verbs;

use geode_core::calibration::CALIBRATION_DOC;
use geode_shell::keymap::KeyContext;
use geode_shell::module::ConfigObject;
use harness::{Harness, builtin_config, dropped_config, open_with, restored, warned_config};

/// A launched or restored tile names its underlying in the title and the
/// configured pipeline's size in the header: eleven stages, two Kalman
/// fits in the builtin `implied_forwards`.
#[gpui::test]
fn a_tile_opens_with_the_launch_underlying_and_titles_itself(cx: &mut gpui::TestAppContext) {
    let (h, vcx) = open_with(
        cx,
        Some(builtin_config()),
        Some(restored(Some("SPX"), None, None)),
    );
    assert_eq!(h.title(&vcx), "Calibration \u{b7} SPX");
    assert_eq!(h.counts(&vcx).as_deref(), Some("11 stages \u{b7} 2 fits"));
    assert_eq!(
        h.body_text(&vcx),
        None,
        "a pipeline is shown, no placeholder"
    );
}

/// Before any config is pushed there is no pipeline to draw: the header
/// has no counts and the body says so.
#[gpui::test]
fn a_tile_without_config_shows_no_pipeline(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, None, Some(restored(Some("SPX"), None, None)));
    assert_eq!(h.title(&vcx), "Calibration \u{b7} SPX");
    assert_eq!(h.counts(&vcx), None);
    assert_eq!(h.body_text(&vcx), Some(crate::tile::NO_PIPELINE));
    h.draw(&mut vcx);
    assert_eq!(Harness::TILE, 7, "the selector below names the tile");
    assert!(
        vcx.debug_bounds("calibration-empty-7").is_some(),
        "the placeholder is painted"
    );
}

/// A config pushed after the tile opened (the startup push races no tile,
/// but a reload reaches open ones) refreshes the header and body.
#[gpui::test]
fn set_config_reaches_an_open_tile(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, None, Some(restored(Some("SPX"), None, None)));
    assert_eq!(h.counts(&vcx), None);
    h.set_config(&mut vcx, builtin_config());
    assert_eq!(h.counts(&vcx).as_deref(), Some("11 stages \u{b7} 2 fits"));
    assert_eq!(h.body_text(&vcx), None);
}

/// The session record carries the underlying, the pipeline and the cursor
/// stage; a restored tile serializes the same table.
#[gpui::test]
fn serialize_round_trips_underlying_pipeline_and_cursor(cx: &mut gpui::TestAppContext) {
    let record = restored(Some("SPX"), Some("implied_forwards"), Some("parity"));
    let (h, vcx) = open_with(cx, Some(builtin_config()), Some(record.clone()));
    assert_eq!(h.serialize(&vcx), record);
}

/// A record without a pipeline means the config's first pipeline, and
/// that is what the tile saves once it knows it.
#[gpui::test]
fn a_missing_pipeline_means_the_first_configured_one(cx: &mut gpui::TestAppContext) {
    let (h, vcx) = open_with(
        cx,
        Some(builtin_config()),
        Some(restored(Some("SPX"), None, Some("parity"))),
    );
    assert_eq!(
        h.serialize(&vcx),
        restored(Some("SPX"), Some("implied_forwards"), Some("parity"))
    );
}

/// The context the keymap resolves against: the module's own flag with
/// `mode == normal`, and no counts (the tile has no counted motion).
#[gpui::test]
fn the_key_context_is_calibration_in_normal_mode(cx: &mut gpui::TestAppContext) {
    let (h, vcx) = open_with(
        cx,
        Some(builtin_config()),
        Some(restored(Some("SPX"), None, None)),
    );
    assert_eq!(
        h.key_context(&vcx),
        KeyContext::new(crate::KIND).pair("mode", "normal")
    );
}

/// A config finding is a header notice; `escape` reaches
/// `calibration::cancel` through the keymap and dismisses it.
#[gpui::test]
fn a_config_finding_is_a_header_notice_that_escape_dismisses(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        Some(warned_config()),
        Some(restored(Some("SPX"), None, None)),
    );
    assert_eq!(
        h.notices(&vcx),
        vec![
            "calibration: pipeline 'p' dropped: stage 'src' reads dataset 'nope', which the \
             schema does not declare"
                .to_string()
        ]
    );
    h.press(&mut vcx, "escape");
    assert_eq!(h.dispatched(), vec!["calibration::cancel".to_string()]);
    assert!(h.notices(&vcx).is_empty(), "dismissed");
}

/// `enter` resolves to the shell's `config::calibration` through the
/// tile's keymap and the tile does not take it (the shell opens the
/// dialog); `config_object` names the cursor's stage so the dialog lands on
/// it.
#[gpui::test]
fn enter_dispatches_the_shell_config_action_and_config_object_is_the_cursor(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_with(
        cx,
        Some(builtin_config()),
        Some(restored(Some("SPX"), None, Some("repo_fit"))),
    );
    assert_eq!(
        h.config_object(&vcx),
        Some(ConfigObject {
            doc: CALIBRATION_DOC,
            name: "repo_fit".into(),
            seed: None,
        })
    );
    h.press(&mut vcx, "enter");
    assert_eq!(h.dispatched(), vec!["config::calibration".to_string()]);
    assert!(
        !h.act(&mut vcx, "config::calibration"),
        "the shell's action, not the tile's"
    );
    assert_eq!(h.cursor(&vcx).as_deref(), Some("repo_fit"), "nothing moved");
}

/// Without a cursor on a stage there is nothing to open: a restored cursor
/// the shown pipeline lacks names no object until a jump lands on a card.
#[gpui::test]
fn config_object_is_none_without_a_cursor_on_a_stage(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        Some(builtin_config()),
        Some(restored(Some("SPX"), None, Some("gone"))),
    );
    assert_eq!(h.config_object(&vcx), None);
    h.press(&mut vcx, "g 1");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("chain"), "the first card");
    assert_eq!(
        h.config_object(&vcx).map(|o| o.name),
        Some("chain".to_string())
    );
}

/// A reload that drops the shown pipeline (a stage the reader refuses)
/// keeps the last valid graph on screen with the first finding as a header
/// notice; `config_object` is `None`, so `enter` reaches the shell's action
/// and opens nothing of this pipeline; the cursor still walks the held
/// graph, and a run is refused since nothing valid is there to run.
#[gpui::test]
fn a_dropped_pipeline_keeps_the_last_graph_and_enter_opens_nothing(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        Some(builtin_config()),
        Some(restored(Some("SPX"), None, Some("repo_fit"))),
    );
    assert_eq!(h.counts(&vcx).as_deref(), Some("11 stages \u{b7} 2 fits"));
    h.set_config(&mut vcx, dropped_config());
    assert_eq!(
        h.counts(&vcx).as_deref(),
        Some("11 stages \u{b7} 2 fits"),
        "the last valid graph stays"
    );
    assert!(
        h.tile.read_with(&vcx, |t, _| t.model().is_some()),
        "the graph is still painted"
    );
    assert_eq!(
        h.notices(&vcx).first().map(String::as_str),
        Some("calibration: stage 'repo_fit': knots is two or more strictly ascending numbers")
    );
    assert_eq!(h.config_object(&vcx), None);
    h.press(&mut vcx, "enter");
    assert_eq!(h.dispatched(), vec!["config::calibration".to_string()]);
    h.press(&mut vcx, "l");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("repo_curve"));
    assert_eq!(
        h.command(&mut vcx, "run"),
        Err(crate::core::menu::NO_PIPELINE.to_string())
    );
    // A valid document again: the live pipeline is back and the cursor's
    // stage is an object once more.
    h.set_config(&mut vcx, builtin_config());
    assert_eq!(
        h.config_object(&vcx).map(|o| o.name),
        Some("repo_curve".to_string())
    );
}
