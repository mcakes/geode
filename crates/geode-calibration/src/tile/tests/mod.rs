//! The tile's window tests: `harness` hosts it as the shell does, with
//! keys resolved through the app's keymap and answers through the
//! content's `deliver`.

mod data;
mod harness;
mod keys;

use geode_shell::keymap::KeyContext;
use harness::{Harness, builtin_config, open_with, restored, warned_config};

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
