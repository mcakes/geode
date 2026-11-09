//! A config push to an open tile (the bridge's reload): the results in
//! hand are kept, a stage whose definition changed goes stale with
//! everything downstream of it, the cursor stays, and the sources are
//! asked again only when a source stage changed.

use geode_core::calibration::BUILTIN_CALIBRATION;
use geode_shell::module::Delivery;

use super::data::{KEY, calibrates, docs, result, run_outcome};
use super::harness::{
    Harness, builtin_config, config_from, config_with_user, dropped_config, open_with, restored,
};
use crate::content::CalibrationConfig;
use crate::core::model::Tone;

/// The builtin document with `repo_fit`'s process noise changed: one
/// Kalman fit's parameters, nothing else.
fn noisier_repo_fit() -> CalibrationConfig {
    let text = BUILTIN_CALIBRATION.replacen(
        "process_noise     = 1.0e-4",
        "process_noise     = 2.0e-4",
        1,
    );
    assert_ne!(text, BUILTIN_CALIBRATION, "the fixture edits repo_fit");
    let config = config_from(&text);
    assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
    config
}

/// The builtin document without `df_monotone`: the stage and the publish's
/// gate input to it.
fn without_check() -> CalibrationConfig {
    let text = BUILTIN_CALIBRATION
        .replace(
            r#"inputs   = ["repo_curve", "box_curve", "df_monotone"]"#,
            r#"inputs   = ["repo_curve", "box_curve"]"#,
        )
        .replace(
            "[stages.df_monotone]\npipeline = \"implied_forwards\"\nkind     = \"check\"\ninputs   = [\"parity\"]\ncheck    = \"monotone_df\"\n",
            "",
        );
    let config = config_from(&text);
    assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
    assert_eq!(config.pipelines.pipelines[0].stages.len(), 10);
    config
}

/// The builtin document without `box_curve`: the stage and the publish's
/// input to it.
fn without_box_curve() -> CalibrationConfig {
    let text = BUILTIN_CALIBRATION
        .replace(
            r#"inputs   = ["repo_curve", "box_curve", "df_monotone"]"#,
            r#"inputs   = ["repo_curve", "df_monotone"]"#,
        )
        .replace(
            "[stages.box_curve]\npipeline = \"implied_forwards\"\nkind     = \"rasterize\"\ninputs   = [\"box_fit\"]\ngrid     = \"monthly_to_5y\"\n",
            "",
        );
    let config = config_from(&text);
    assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
    assert_eq!(config.pipelines.pipelines[0].stages.len(), 10);
    config
}

/// The builtin document with the `rates` source reading another series.
fn other_rates_series() -> CalibrationConfig {
    let text = BUILTIN_CALIBRATION.replace("series   = \"funding\"", "series   = \"discount\"");
    assert_ne!(text, BUILTIN_CALIBRATION, "the fixture edits rates");
    let config = config_from(&text);
    assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
    config
}

const ALL: [&str; 11] = [
    "chain",
    "dividends",
    "rates",
    "parity",
    "implied_rates",
    "df_monotone",
    "repo_fit",
    "box_fit",
    "repo_curve",
    "box_curve",
    "publish",
];

impl Harness {
    /// A loaded tile whose every stage has run once: the sources answered,
    /// the run taken and answered with an `Ok` result per stage.
    fn ran(&self, vcx: &mut gpui::VisualTestContext) -> u64 {
        let tag = self.load(vcx, "SPX");
        self.deliver(
            vcx,
            Delivery::Calibration(run_outcome(
                KEY,
                tag,
                ALL.iter().map(|s| result(s, "ok")).collect(),
            )),
        );
        for stage in ALL {
            assert_eq!(self.card_tone(vcx, stage), Tone::Plain, "{stage}");
            assert_eq!(self.card_status(vcx, stage), "09:30:15 · ok");
        }
        tag
    }

    fn tones(&self, vcx: &gpui::VisualTestContext) -> Vec<(&'static str, Tone)> {
        ALL.iter().map(|s| (*s, self.card_tone(vcx, s))).collect()
    }
}

fn open(cx: &mut gpui::TestAppContext, cursor: Option<&str>) -> (Harness, gpui::VisualTestContext) {
    open_with(
        cx,
        Some(builtin_config()),
        Some(restored(Some("SPX"), None, cursor)),
    )
}

/// `repo_fit`'s parameters change: it and everything downstream of it
/// (`repo_curve`, `publish`) paint stale, keeping their time and summary
/// until the next outcome; every other card keeps its result and tone. A
/// stage the new pipeline lacks loses its result; a new stage has none;
/// nothing is asked again, since no source changed.
#[gpui::test]
fn a_config_push_with_a_changed_stage_marks_it_stale_and_keeps_the_rest(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx, Some("parity"));
    let tag = h.ran(&mut vcx);

    h.set_config(&mut vcx, noisier_repo_fit());
    let stale = ["repo_fit", "repo_curve", "publish"];
    for (stage, tone) in h.tones(&vcx) {
        let expected = if stale.contains(&stage) {
            Tone::Stale
        } else {
            Tone::Plain
        };
        assert_eq!(tone, expected, "{stage}");
        assert_eq!(
            h.card_status(&vcx, stage),
            "09:30:15 · ok",
            "{stage} keeps its time and summary"
        );
    }
    assert_eq!(h.result_names(&vcx).len(), 11, "every result is kept");
    assert!(
        h.requests().is_empty(),
        "no source changed: nothing is asked"
    );
    assert_eq!(h.footer(&vcx), "parity · 09:30:15 · ok");

    // The run that answered before the push answers again (a late
    // delivery under its tag): it was computed over the old definition,
    // so it is dropped and the stale cards stand as they are.
    h.deliver(
        &mut vcx,
        Delivery::Calibration(run_outcome(KEY, tag, vec![result("repo_fit", "late")])),
    );
    assert_eq!(
        h.card_tone(&vcx, "repo_fit"),
        Tone::Stale,
        "a late run never clears a mark"
    );
    assert_eq!(h.card_status(&vcx, "repo_fit"), "09:30:15 · ok");

    // The next outcome for a stale stage replaces the mark; the rest of
    // the stale set waits for its own.
    h.press(&mut vcx, "r");
    assert!(h.notices(&vcx).is_empty(), "{:?}", h.notices(&vcx));
    let reqs = h.requests();
    let runs = calibrates(&reqs);
    assert_eq!(runs.len(), 1, "{reqs:?}");
    assert!(runs[0].tag > tag);
    h.deliver(
        &mut vcx,
        Delivery::Calibration(run_outcome(
            KEY,
            runs[0].tag,
            vec![result("repo_fit", "again")],
        )),
    );
    assert_eq!(h.card_tone(&vcx, "repo_fit"), Tone::Plain);
    assert_eq!(h.card_status(&vcx, "repo_fit"), "09:30:15 · again");
    assert_eq!(h.card_tone(&vcx, "repo_curve"), Tone::Stale);
    assert_eq!(h.card_tone(&vcx, "publish"), Tone::Stale);

    // A stage gone from the pipeline: its result goes; the publish, whose
    // inputs changed, is stale (it already was). A stage added has not run.
    h.set_config(&mut vcx, without_check());
    assert_eq!(h.counts(&vcx).as_deref(), Some("10 stages \u{b7} 2 fits"));
    assert!(
        !h.result_names(&vcx).iter().any(|n| n == "df_monotone"),
        "{:?}",
        h.result_names(&vcx)
    );
    assert_eq!(h.card_tone(&vcx, "publish"), Tone::Stale);
    assert_eq!(h.card_tone(&vcx, "parity"), Tone::Plain);
    assert_eq!(h.card_status(&vcx, "parity"), "09:30:15 · ok");

    let user = config_with_user(
        "[stages.check2]\npipeline = \"implied_forwards\"\nkind = \"check\"\ninputs = [\"parity\"]\ncheck = \"monotone_df\"\n",
    );
    assert!(user.diagnostics.is_empty(), "{:?}", user.diagnostics);
    h.set_config(&mut vcx, user);
    assert_eq!(h.counts(&vcx).as_deref(), Some("12 stages \u{b7} 2 fits"));
    assert_eq!(h.card_status(&vcx, "check2"), "not run");
    assert_eq!(h.card_tone(&vcx, "check2"), Tone::Plain);
    // `df_monotone` is back, as a stage with no result; the publish's inputs
    // changed back and it stays stale.
    assert_eq!(h.card_status(&vcx, "df_monotone"), "not run");
    assert_eq!(h.card_status(&vcx, "parity"), "09:30:15 · ok");
    assert!(h.requests().is_empty(), "still no source change");
}

/// A reload that drops the shown pipeline keeps the last valid graph with
/// the finding as a notice: the results, their tones and the cursor stand
/// (nothing is stale, since nothing valid replaced the definition they
/// were computed for), and nothing is asked. A valid document again
/// compares against that definition: unchanged, nothing is stale; changed,
/// the changed stage is.
#[gpui::test]
fn a_config_push_that_drops_the_pipeline_keeps_the_last_graph_with_a_notice(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx, Some("repo_fit"));
    h.ran(&mut vcx);

    h.set_config(&mut vcx, dropped_config());
    assert_eq!(h.counts(&vcx).as_deref(), Some("11 stages \u{b7} 2 fits"));
    assert_eq!(
        h.notices(&vcx).first().map(String::as_str),
        Some("calibration: stage 'repo_fit': knots is two or more strictly ascending numbers")
    );
    for (stage, tone) in h.tones(&vcx) {
        assert_eq!(tone, Tone::Plain, "{stage} stands as it ran");
        assert_eq!(h.card_status(&vcx, stage), "09:30:15 · ok");
    }
    assert_eq!(h.cursor(&vcx).as_deref(), Some("repo_fit"));
    assert_eq!(h.result_names(&vcx).len(), 11);
    assert!(h.requests().is_empty(), "{:?}", h.requests());
    assert_eq!(h.body_text(&vcx), None, "the graph is still painted");

    // The document fixed as it was: nothing changed, nothing is stale.
    h.set_config(&mut vcx, builtin_config());
    assert!(h.notices(&vcx).is_empty());
    for (stage, tone) in h.tones(&vcx) {
        assert_eq!(tone, Tone::Plain, "{stage}");
    }
    assert!(h.requests().is_empty());

    // Dropped again, then fixed with `repo_fit` changed: the comparison is
    // against the definition the results were computed for.
    h.set_config(&mut vcx, dropped_config());
    h.set_config(&mut vcx, noisier_repo_fit());
    assert_eq!(h.card_tone(&vcx, "repo_fit"), Tone::Stale);
    assert_eq!(h.card_tone(&vcx, "repo_curve"), Tone::Stale);
    assert_eq!(h.card_tone(&vcx, "publish"), Tone::Stale);
    assert_eq!(h.card_tone(&vcx, "box_fit"), Tone::Plain);
    assert_eq!(h.cursor(&vcx).as_deref(), Some("repo_fit"));
}

/// The cursor survives every push: a changed stage, added stages, a dropped
/// pipeline; only a cursor on a stage the new pipeline lacks moves, to the
/// first card in layout order. The filter in force survives too.
#[gpui::test]
fn a_config_push_never_resets_the_cursor(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx, Some("box_curve"));
    h.ran(&mut vcx);
    h.press(&mut vcx, "/");
    vcx.simulate_input("curve");
    vcx.run_until_parked();
    h.press(&mut vcx, "enter");
    assert_eq!(
        h.cursor(&vcx).as_deref(),
        Some("repo_curve"),
        "enter lands on the first lit card"
    );
    h.press(&mut vcx, "j");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("box_curve"));
    let filter = |h: &Harness, vcx: &gpui::VisualTestContext| {
        h.tile.read_with(vcx, |t, _| t.filter().map(str::to_string))
    };
    assert_eq!(filter(&h, &vcx).as_deref(), Some("curve"));

    h.set_config(&mut vcx, noisier_repo_fit());
    assert_eq!(h.cursor(&vcx).as_deref(), Some("box_curve"));
    assert_eq!(filter(&h, &vcx).as_deref(), Some("curve"));
    assert_eq!(h.footer(&vcx), "box_curve · 09:30:15 · ok");

    let user = config_with_user(
        "[stages.check2]\npipeline = \"implied_forwards\"\nkind = \"check\"\ninputs = [\"parity\"]\ncheck = \"monotone_df\"\n",
    );
    h.set_config(&mut vcx, user);
    assert_eq!(h.cursor(&vcx).as_deref(), Some("box_curve"));

    h.set_config(&mut vcx, dropped_config());
    assert_eq!(h.cursor(&vcx).as_deref(), Some("box_curve"));

    // The pipeline back without `box_curve`: the cursor cannot stay on a
    // card that is gone, so it takes the first card in layout order.
    h.set_config(&mut vcx, without_box_curve());
    assert_eq!(h.cursor(&vcx).as_deref(), Some("chain"));
    assert_eq!(
        h.serialize(&vcx),
        restored(Some("SPX"), Some("implied_forwards"), Some("chain"))
    );
    assert_eq!(
        filter(&h, &vcx).as_deref(),
        Some("curve"),
        "the filter stands"
    );
    assert_eq!(
        h.card_status(&vcx, "parity"),
        "09:30:15 · ok",
        "the results stand"
    );
}

/// Only a source stage's change (its dataset or series, or a source added)
/// asks the documents again; any other change leaves the documents in
/// hand. The changed source and everything downstream go stale, and the
/// run that follows the new documents replaces them. Hidden, the ask waits
/// for the show, as every ask does.
#[gpui::test]
fn a_config_push_that_changes_a_source_requeries_only_then(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx, Some("parity"));
    let tag = h.ran(&mut vcx);

    h.set_config(&mut vcx, noisier_repo_fit());
    assert!(h.requests().is_empty(), "a fit's change asks nothing");

    h.set_config(&mut vcx, other_rates_series());
    let reqs = h.requests();
    assert_eq!(
        docs(&reqs).len(),
        1,
        "the sources are asked again: {reqs:?}"
    );
    assert!(
        calibrates(&reqs).is_empty(),
        "no run before the documents: {reqs:?}"
    );
    // The sources wait while the reads are out; the stages fed by `rates`
    // are stale; the stages beside them stand.
    for stage in ["chain", "dividends", "rates"] {
        assert_eq!(h.card_tone(&vcx, stage), Tone::Waiting, "{stage}");
    }
    for stage in ["implied_rates", "box_fit", "box_curve", "publish"] {
        assert_eq!(h.card_tone(&vcx, stage), Tone::Stale, "{stage}");
    }
    for stage in ["parity", "df_monotone"] {
        assert_eq!(h.card_tone(&vcx, stage), Tone::Plain, "{stage}");
        assert_eq!(h.card_status(&vcx, stage), "09:30:15 · ok");
    }
    assert_eq!(h.footer(&vcx), "waiting for chain, dividends, rates");
    let (asked, after) = h.answer_all_from(&mut vcx, reqs, "SPX");
    assert_eq!(asked.len(), 3, "{asked:?}");
    let runs = calibrates(&after);
    assert_eq!(
        runs.len(),
        1,
        "the documents in, the run goes out: {after:?}"
    );
    assert!(runs[0].tag > tag);
    assert_eq!(h.card_tone(&vcx, "rates"), Tone::Stale, "until its outcome");
    h.deliver(
        &mut vcx,
        Delivery::Calibration(run_outcome(
            KEY,
            runs[0].tag,
            vec![
                result("rates", "discount"),
                result("implied_rates", "again"),
            ],
        )),
    );
    assert_eq!(h.card_tone(&vcx, "rates"), Tone::Plain);
    assert_eq!(h.card_status(&vcx, "rates"), "09:30:15 · discount");
    assert_eq!(h.card_tone(&vcx, "implied_rates"), Tone::Plain);
    assert_eq!(
        h.card_tone(&vcx, "box_fit"),
        Tone::Stale,
        "its own outcome is owed"
    );

    // Hidden: a source change asks nothing until the tile is shown, and
    // the stale marks are made at once.
    h.set_visible(&mut vcx, false);
    h.set_config(&mut vcx, builtin_config());
    assert!(h.requests().is_empty(), "hidden: nothing is asked");
    assert_eq!(
        h.card_tone(&vcx, "rates"),
        Tone::Waiting,
        "the old series' document is owed"
    );
    assert_eq!(h.card_tone(&vcx, "implied_rates"), Tone::Stale);
    h.set_visible(&mut vcx, true);
    let reqs = h.requests();
    assert_eq!(
        docs(&reqs).len(),
        1,
        "shown: the sources are asked: {reqs:?}"
    );
    assert!(calibrates(&reqs).is_empty(), "{reqs:?}");
}
