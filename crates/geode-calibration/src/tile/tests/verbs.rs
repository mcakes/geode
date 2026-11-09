//! The verbs: `r` and `shift+r` run, `p` publishes through the y/n bar
//! and the uploads that follow, the `.` action menu and the `:` line.
//! Every key goes through the app's keymap, every answer through
//! `TileContent::deliver`, every request is read off the data handle's
//! receiver.

use std::time::Instant;

use geode_core::calibration::{
    BUILTIN_CALIBRATION, CalibrationOutcome, PublishSource, RunAction, StageResult, StageStatus,
};
use geode_core::document::DocumentRows;
use geode_core::query::QueryKey;
use geode_data::{Request, UploadParams};
use geode_shell::actions::ActionId;
use geode_shell::keymap::{KeyContext, Keystroke, Modifiers, parse_binding};
use geode_shell::module::{Delivery, UploadDelivery};
use geode_shell::tiling::TileId;
use geode_tile::menu::Lane;
use gpui::Modifiers as MouseModifiers;

use super::data::{KEY, calibrates, docs, rates_doc, result, run_outcome};
use super::harness::{Harness, builtin_config, config_from, open_with, restored};
use crate::content::CalibrationConfig;
use crate::core::menu::OPEN_STAGE;
use crate::core::model::{EdgeTone, Tone};
use crate::tile::data::QUEUE_FULL;

const HELD: &str = "df_monotone: 1 violation";

fn open_spx(
    cx: &mut gpui::TestAppContext,
    cursor: Option<&str>,
) -> (Harness, gpui::VisualTestContext) {
    open_with(
        cx,
        Some(builtin_config()),
        Some(restored(Some("SPX"), None, cursor)),
    )
}

fn uploads(reqs: &[Request]) -> Vec<&UploadParams> {
    reqs.iter()
        .filter_map(|r| match r {
            Request::Upload(p) => Some(p),
            _ => None,
        })
        .collect()
}

fn held(name: &str, reason: &str) -> StageResult {
    StageResult {
        status: StageStatus::Held(reason.into()),
        ..result(name, reason)
    }
}

/// A publish's answer: the publish stage and one document per series.
fn publish_outcome(
    tag: u64,
    stages: Vec<StageResult>,
    rows: Vec<DocumentRows>,
) -> CalibrationOutcome {
    CalibrationOutcome {
        key: KEY,
        tag,
        submitted: Instant::now(),
        action: RunAction::Publish {
            from: PublishSource::Filter,
        },
        stages,
        publish_rows: Some(rows),
    }
}

fn upload_answer(tag: u64, result: Result<(), &str>) -> Delivery {
    Delivery::Upload(UploadDelivery {
        key: KEY,
        tag,
        target: "sophis".into(),
        result: result.map_err(str::to_string),
    })
}

/// The builtin document without its publish stage: ten stages, no target.
fn no_publish_config() -> CalibrationConfig {
    let text = BUILTIN_CALIBRATION
        .split("[stages.publish]")
        .next()
        .expect("the publish stage is the last block");
    let config = config_from(text);
    assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
    config
}

/// The builtin document plus `funding_only`: one source stage over the
/// rate curve, no fit, no publish.
fn two_pipeline_config() -> CalibrationConfig {
    let text = format!(
        "{BUILTIN_CALIBRATION}\n[pipelines.funding_only]\n\
         [stages.funding]\npipeline = \"funding_only\"\nkind = \"source\"\n\
         dataset = \"rate_curve\"\n"
    );
    let config = config_from(&text);
    assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
    assert_eq!(config.pipelines.pipelines.len(), 2);
    config
}

fn keys(spec: &str) -> Vec<Keystroke> {
    parse_binding(spec, Modifiers::NONE).unwrap()
}

fn normal() -> KeyContext {
    KeyContext::new(crate::KIND).pair("mode", "normal")
}

fn insert() -> KeyContext {
    KeyContext::new(crate::KIND).pair("mode", "insert")
}

fn menu() -> KeyContext {
    KeyContext::new(crate::KIND)
        .pair("mode", "menu")
        .pair("popup", "actions")
        .tilelist()
}

/// `HH:MM:SS`, as the clock prints a time.
fn is_hms(s: &str) -> bool {
    s.len() == 8
        && s.as_bytes()[2] == b':'
        && s.as_bytes()[5] == b':'
        && s.chars()
            .enumerate()
            .all(|(i, c)| matches!(i, 2 | 5) || c.is_ascii_digit())
}

impl Harness {
    pub(super) fn confirm_text(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile.read_with(vcx, |t, _| t.confirm_text())
    }

    fn menu_open(&self, vcx: &gpui::VisualTestContext) -> Option<(Vec<String>, Option<usize>)> {
        self.tile.read_with(vcx, |t, _| t.menu_open())
    }

    fn menu_lane(&self, vcx: &gpui::VisualTestContext, id: &str) -> Option<Lane> {
        self.tile.read_with(vcx, |t, _| t.menu_lane(id))
    }

    fn menu_reason(&self, vcx: &gpui::VisualTestContext, id: &str) -> Option<String> {
        self.tile.read_with(vcx, |t, _| t.menu_reason(id))
    }

    fn click(&self, vcx: &mut gpui::VisualTestContext, selector: &'static str) {
        self.draw(vcx);
        let bounds = vcx.debug_bounds(selector);
        let at = bounds.expect("the selector is painted").center();
        vcx.simulate_click(at, MouseModifiers::default());
        vcx.run_until_parked();
        self.draw(vcx);
    }

    /// A card's whole status line, as the tooltip and the footer show it.
    fn card_status_full(&self, vcx: &gpui::VisualTestContext, stage: &str) -> String {
        self.tile.read_with(vcx, |t, _| {
            t.model()
                .expect("a pipeline is shown")
                .cards
                .iter()
                .find(|c| c.stage == stage)
                .unwrap_or_else(|| panic!("{stage} has no card"))
                .status_full
                .to_string()
        })
    }

    /// The painted tone of the edge `from` -> `to`, `None` with no such
    /// edge.
    fn edge_tone(&self, vcx: &gpui::VisualTestContext, from: &str, to: &str) -> Option<EdgeTone> {
        self.tile.read_with(vcx, |t, _| {
            t.model()?
                .edges
                .iter()
                .find(|e| e.from == from && e.to == to)
                .map(|e| e.tone)
        })
    }

    /// `p`, `y`, the publish answered with two documents: the tags of the
    /// two uploads that go out, in the answer's order.
    fn publish_two(&self, vcx: &mut gpui::VisualTestContext) -> Vec<u64> {
        self.press(vcx, "p");
        self.press(vcx, "y");
        let reqs = self.requests();
        let publish = calibrates(&reqs);
        assert_eq!(publish.len(), 1, "{reqs:?}");
        self.deliver(
            vcx,
            Delivery::Calibration(publish_outcome(
                publish[0].tag,
                vec![result("publish", "2 series")],
                vec![rates_doc("SPX", "repo"), rates_doc("SPX", "box")],
            )),
        );
        let reqs = self.requests();
        let ups = uploads(&reqs);
        assert_eq!(ups.len(), 2, "{reqs:?}");
        ups.iter().map(|u| u.tag).collect()
    }
}

/// `r` runs from the cursor's stage, `shift+r` the whole pipeline, each
/// over the cached inputs under a new tag; a refused submission is a
/// notice and `r` with no cursor says so.
#[gpui::test]
fn r_runs_from_the_cursor_and_shift_r_runs_everything(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx, Some("parity"));
    let first = h.load(&mut vcx, "SPX");
    h.press(&mut vcx, "r");
    let reqs = h.requests();
    let runs = calibrates(&reqs);
    assert_eq!(runs.len(), 1, "{reqs:?}");
    assert_eq!(
        runs[0].action,
        RunAction::Run {
            from: Some("parity".into())
        }
    );
    assert!(runs[0].tag > first, "a new tag");
    assert_eq!(runs[0].inputs.len(), 3, "the cached inputs ride along");
    assert_eq!(runs[0].underlying, "SPX");
    h.press(&mut vcx, "shift-r");
    let reqs = h.requests();
    let runs = calibrates(&reqs);
    assert_eq!(runs.len(), 1, "{reqs:?}");
    assert_eq!(runs[0].action, RunAction::Run { from: None });
    assert_eq!(
        h.dispatched(),
        ["calibration::run_from", "calibration::run_all"]
    );
    assert!(h.notices(&vcx).is_empty());
    // The run's answer lands on the run lane.
    h.deliver(
        &mut vcx,
        Delivery::Calibration(run_outcome(
            KEY,
            runs[0].tag,
            vec![result("parity", "6 strikes")],
        )),
    );
    assert_eq!(h.card_status(&vcx, "parity"), "09:30:15 · 6 strikes");
    // A full queue refuses with its notice; the documents are kept.
    h.data.fill_for_tests();
    h.press(&mut vcx, "r");
    assert_eq!(h.notices(&vcx), vec![QUEUE_FULL.to_string()]);
    assert_eq!(h.footer(&vcx), "parity · 09:30:15 · 6 strikes");
}

/// `r` needs a stage under the cursor; the refusal says so and nothing
/// goes out.
#[gpui::test]
fn r_without_a_cursor_is_refused(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx, None);
    h.load(&mut vcx, "SPX");
    h.press(&mut vcx, "r");
    assert_eq!(
        h.notices(&vcx),
        vec!["no stage under the cursor".to_string()]
    );
    assert!(h.requests().is_empty());
    // `shift+r` needs none.
    h.press(&mut vcx, "shift-r");
    assert_eq!(calibrates(&h.requests()).len(), 1);
}

/// `p` arms the bar naming the series, the underlying and the target and
/// sends nothing; `y` submits the publish from the filter state with the
/// cached inputs; the answer's documents each go to the target under the
/// tile's upload counter. A run's answer never carries rows to upload.
#[gpui::test]
fn p_arms_the_confirm_bar_and_y_publishes_from_the_filter(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx, Some("publish"));
    let run_tag = h.load(&mut vcx, "SPX");
    h.press(&mut vcx, "p");
    assert_eq!(h.dispatched(), ["calibration::publish"]);
    assert_eq!(
        h.confirm_text(&vcx).as_deref(),
        Some("Publish repo, box for SPX to sophis? y/n")
    );
    assert_eq!(h.key_context(&vcx), insert(), "the bar holds the keys");
    h.draw(&mut vcx);
    assert!(
        vcx.debug_bounds("calibration-confirm-7-bar").is_some(),
        "the bar is painted under the header"
    );
    assert!(h.requests().is_empty(), "nothing goes out before y");

    h.press(&mut vcx, "y");
    assert_eq!(h.confirm_text(&vcx), None);
    assert_eq!(h.key_context(&vcx), normal());
    let reqs = h.requests();
    let publish = calibrates(&reqs);
    assert_eq!(publish.len(), 1, "{reqs:?}");
    assert_eq!(
        publish[0].action,
        RunAction::Publish {
            from: PublishSource::Filter
        }
    );
    assert!(publish[0].tag > run_tag, "the user lane takes the next tag");
    assert_eq!(publish[0].key, KEY);
    assert_eq!(publish[0].underlying, "SPX");
    assert_eq!(publish[0].inputs.len(), 3);
    assert_eq!(h.dispatched().len(), 1, "y was the bar's, not the keymap's");

    h.deliver(
        &mut vcx,
        Delivery::Calibration(publish_outcome(
            publish[0].tag,
            vec![result("publish", "2 series")],
            vec![rates_doc("SPX", "repo"), rates_doc("SPX", "box")],
        )),
    );
    let reqs = h.requests();
    let ups = uploads(&reqs);
    assert_eq!(ups.len(), 2, "{reqs:?}");
    for u in &ups {
        assert_eq!(u.key, KEY);
        assert_eq!(u.target, "sophis");
        assert_eq!(u.document, "rate_curve");
    }
    assert_eq!(ups[0].rows.key, vec!["SPX".to_string(), "repo".to_string()]);
    assert_eq!(ups[1].rows.key, vec!["SPX".to_string(), "box".to_string()]);
    assert_ne!(ups[0].tag, ups[1].tag, "one upload tag each");
    assert_eq!(h.card_status(&vcx, "publish"), "09:30:15 · 2 series");

    // Rows on a run's answer are not a publish's: nothing is uploaded.
    h.deliver(
        &mut vcx,
        Delivery::Calibration(CalibrationOutcome {
            publish_rows: Some(vec![rates_doc("SPX", "repo")]),
            ..run_outcome(KEY, run_tag, vec![result("parity", "6 strikes")])
        }),
    );
    let reqs = h.requests();
    assert!(uploads(&reqs).is_empty(), "{reqs:?}");
    assert_eq!(h.card_status(&vcx, "parity"), "09:30:15 · 6 strikes");
}

/// `p` refuses, naming the object, while the publish stage is held (its
/// reason), while a publish is in flight, and when the pipeline has no
/// publish stage; nothing is armed and nothing goes out.
#[gpui::test]
fn p_is_refused_while_publish_is_held_or_in_flight_or_absent(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx, None);
    let tag = h.load(&mut vcx, "SPX");
    h.deliver(
        &mut vcx,
        Delivery::Calibration(run_outcome(KEY, tag, vec![held("publish", HELD)])),
    );
    h.press(&mut vcx, "p");
    assert_eq!(h.notices(&vcx), vec![format!("publish is held: {HELD}")]);
    assert_eq!(h.confirm_text(&vcx), None);
    assert!(h.requests().is_empty());

    // The hold lifted: `p` arms and `y` sends.
    h.deliver(
        &mut vcx,
        Delivery::Calibration(run_outcome(KEY, tag, vec![result("publish", "2 series")])),
    );
    h.press(&mut vcx, "p");
    assert!(h.confirm_text(&vcx).is_some());
    assert!(h.notices(&vcx).is_empty(), "the refusal left with the ask");
    h.press(&mut vcx, "y");
    let reqs = h.requests();
    let publish = calibrates(&reqs);
    assert_eq!(publish.len(), 1, "{reqs:?}");
    // In flight: a second `p` is refused.
    h.press(&mut vcx, "p");
    assert_eq!(
        h.notices(&vcx),
        vec!["publish already in flight".to_string()]
    );
    assert_eq!(h.confirm_text(&vcx), None);
    assert!(h.requests().is_empty());
    // Answered (with nothing to upload): the next `p` asks again, and `n`
    // sends nothing.
    h.deliver(
        &mut vcx,
        Delivery::Calibration(publish_outcome(
            publish[0].tag,
            vec![result("publish", "0 series")],
            Vec::new(),
        )),
    );
    h.press(&mut vcx, "p");
    assert!(h.confirm_text(&vcx).is_some());
    h.press(&mut vcx, "n");
    assert_eq!(h.confirm_text(&vcx), None);
    assert!(h.requests().is_empty());

    // No publish stage: refused by name.
    h.set_config(&mut vcx, no_publish_config());
    assert_eq!(h.counts(&vcx).as_deref(), Some("10 stages \u{b7} 2 fits"));
    h.press(&mut vcx, "p");
    assert!(
        h.notices(&vcx)
            .contains(&"no publish stage in implied_forwards".to_string()),
        "{:?}",
        h.notices(&vcx)
    );
    assert_eq!(h.confirm_text(&vcx), None);
}

/// An upload's answer is a notice naming the document, its key, the
/// target and the time. The publish card trails the time only once every
/// document of the publish has landed; a failure keeps it off the card and
/// says what failed; the engine's words on the card are never touched; a
/// stale or foreign tag is dropped.
#[gpui::test]
fn an_upload_result_is_a_notice_and_updates_the_publish_card(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx, None);
    h.load(&mut vcx, "SPX");
    let ups = h.publish_two(&mut vcx);
    assert_eq!(h.card_status_full(&vcx, "publish"), "09:30:15 · 2 series");

    h.deliver(&mut vcx, upload_answer(ups[0], Ok(())));
    let notices = h.notices(&vcx);
    assert_eq!(notices.len(), 1, "{notices:?}");
    let (text, time) = notices[0]
        .rsplit_once(' ')
        .expect("the time ends the notice");
    assert_eq!(text, "uploaded rate_curve SPX/repo to sophis");
    assert!(is_hms(time), "{time}");
    assert_eq!(
        h.card_status_full(&vcx, "publish"),
        "09:30:15 · 2 series",
        "one of two landed: no time on the card yet"
    );

    h.deliver(&mut vcx, upload_answer(ups[1], Err("sophis is down")));
    let notices = h.notices(&vcx);
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert_eq!(
        notices[1],
        "upload rate_curve SPX/box to sophis failed: sophis is down"
    );
    assert_eq!(
        h.card_status_full(&vcx, "publish"),
        "09:30:15 · 2 series",
        "a failed upload keeps the time off the card"
    );
    assert_eq!(h.card_tone(&vcx, "publish"), Tone::Plain);

    // Stale or another tile's: dropped.
    h.deliver(&mut vcx, upload_answer(ups[1] + 7, Ok(())));
    h.deliver(
        &mut vcx,
        Delivery::Upload(UploadDelivery {
            key: QueryKey(99),
            tag: ups[0],
            target: "sophis".into(),
            result: Ok(()),
        }),
    );
    assert_eq!(h.notices(&vcx).len(), 2);
    // `escape` dismisses the failure; the status stays.
    h.press(&mut vcx, "escape");
    assert_eq!(h.notices(&vcx).len(), 1);

    // A second publish, every document landing: the card trails the time
    // after the engine's words, which stand.
    let ups = h.publish_two(&mut vcx);
    assert!(
        h.notices(&vcx).is_empty(),
        "a new publish clears the last one's upload notices"
    );
    h.deliver(&mut vcx, upload_answer(ups[0], Ok(())));
    h.deliver(&mut vcx, upload_answer(ups[1], Ok(())));
    let notices = h.notices(&vcx);
    assert_eq!(notices.len(), 2, "{notices:?}");
    let (_, time) = notices[1].rsplit_once(' ').unwrap();
    assert!(is_hms(time), "{time}");
    let full = h.card_status_full(&vcx, "publish");
    assert_eq!(full, format!("09:30:15 · 2 series · uploaded {time}"));
    assert_ne!(
        h.card_status(&vcx, "publish"),
        full,
        "the long line is cut for the card and whole in the tooltip"
    );
    assert_eq!(h.result_names(&vcx), vec!["publish"], "no result was added");

    // A third publish: its answer drops the last publish's time from the
    // card, and with one of its documents down the time never returns.
    let ups = h.publish_two(&mut vcx);
    assert_eq!(
        h.card_status_full(&vcx, "publish"),
        "09:30:15 · 2 series",
        "the last publish's time went with its answer"
    );
    h.deliver(&mut vcx, upload_answer(ups[0], Err("sophis is down")));
    h.deliver(&mut vcx, upload_answer(ups[1], Ok(())));
    assert_eq!(
        h.card_status_full(&vcx, "publish"),
        "09:30:15 · 2 series",
        "one document down: no time on the card"
    );
    assert_eq!(h.notices(&vcx).len(), 2);
}

/// A late upload success never lifts a hold. After the publish answers
/// and its uploads are out, the pipeline runs again and the engine holds
/// the publish; the uploads then landing trail their time on the card,
/// but the result is the engine's: the card is held, its gate paints and
/// `p` is still refused.
#[gpui::test]
fn a_late_upload_never_rewrites_a_held_publish(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx, Some("publish"));
    h.load(&mut vcx, "SPX");
    let ups = h.publish_two(&mut vcx);
    h.press(&mut vcx, "shift-r");
    let reqs = h.requests();
    let runs = calibrates(&reqs);
    assert_eq!(runs.len(), 1, "{reqs:?}");
    h.deliver(
        &mut vcx,
        Delivery::Calibration(run_outcome(KEY, runs[0].tag, vec![held("publish", HELD)])),
    );
    assert_eq!(h.card_tone(&vcx, "publish"), Tone::Held);

    h.deliver(&mut vcx, upload_answer(ups[0], Ok(())));
    h.deliver(&mut vcx, upload_answer(ups[1], Ok(())));
    assert_eq!(
        h.card_tone(&vcx, "publish"),
        Tone::Held,
        "the engine's result stands"
    );
    let full = h.card_status_full(&vcx, "publish");
    assert!(
        full.starts_with(&format!("09:30:15 · {HELD} · uploaded ")),
        "{full}"
    );
    assert_eq!(
        h.edge_tone(&vcx, "df_monotone", "publish"),
        Some(EdgeTone::Gate),
        "the gate still paints"
    );
    h.press(&mut vcx, "p");
    assert_eq!(h.confirm_text(&vcx), None);
    assert!(
        h.notices(&vcx)
            .contains(&format!("publish is held: {HELD}")),
        "{:?}",
        h.notices(&vcx)
    );
    assert!(calibrates(&h.requests()).is_empty());
}

/// A question armed over SPX dies with SPX: the followed group's scope
/// moving to NDX withdraws it unanswered, and `y` afterwards publishes
/// nothing, even once NDX's sources are in and its run has gone out.
#[gpui::test]
fn a_publish_question_dies_with_its_underlying(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, Some(builtin_config()), None);
    h.follow_a(&mut vcx);
    h.flip_to(&mut vcx, "SPX");
    h.load(&mut vcx, "SPX");
    h.press(&mut vcx, "p");
    assert_eq!(
        h.confirm_text(&vcx).as_deref(),
        Some("Publish repo, box for SPX to sophis? y/n")
    );
    let keys_before = h.dispatched().len();

    h.flip_to(&mut vcx, "NDX");
    assert_eq!(h.title(&vcx), "Calibration \u{b7} NDX");
    assert_eq!(h.confirm_text(&vcx), None, "withdrawn with SPX");
    assert_eq!(h.key_context(&vcx), normal());
    let (asked, after) = h.answer_all(&mut vcx, "NDX");
    assert_eq!(asked.len(), 3);
    let runs = calibrates(&after);
    assert_eq!(runs.len(), 1, "{after:?}");
    assert_eq!(runs[0].underlying, "NDX");
    assert_eq!(runs[0].action, RunAction::Run { from: None });

    h.press(&mut vcx, "y");
    let reqs = h.requests();
    assert!(reqs.is_empty(), "y publishes nothing: {reqs:?}");
    assert_eq!(h.confirm_text(&vcx), None);
    assert_eq!(h.dispatched().len(), keys_before, "y reached no binding");
    assert!(h.notices(&vcx).is_empty(), "{:?}", h.notices(&vcx));
}

/// `.` opens the menu: the verbs in order with their live chords, the
/// rows that cannot act yet saying so; `j` steps, `enter` picks through
/// the tile's dispatch; `.` and `escape` close it; the header's `⋯`
/// opens the same menu; a disabled row's pick is its reason.
#[gpui::test]
fn the_menu_lists_the_verbs_with_live_chords_and_picks_dispatch(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx, Some("parity"));
    let tag = h.load(&mut vcx, "SPX");
    h.press(&mut vcx, ".");
    assert_eq!(h.dispatched(), ["calibration::menu"]);
    let (rows, highlighted) = h.menu_open(&vcx).expect("the menu is up");
    assert_eq!(
        rows,
        [
            "calibration::run_from",
            "calibration::run_all",
            "calibration::publish",
            OPEN_STAGE,
            "calibration::connect",
            "calibration::disconnect",
            "calibration::add",
            "calibration::delete",
        ]
    );
    assert_eq!(highlighted, Some(0), "the first enabled row");
    assert_eq!(h.key_context(&vcx), menu());
    assert_eq!(
        h.menu_lane(&vcx, "calibration::publish"),
        Some(Lane::Keys(keys("p")))
    );
    assert_eq!(
        h.menu_lane(&vcx, "calibration::run_all"),
        Some(Lane::Keys(keys("shift+r")))
    );
    assert_eq!(h.menu_reason(&vcx, "calibration::run_from"), None);
    assert_eq!(
        h.menu_reason(&vcx, OPEN_STAGE),
        None,
        "the cursor is on a stage of a valid pipeline"
    );
    for id in [
        "calibration::connect",
        "calibration::disconnect",
        "calibration::add",
        "calibration::delete",
    ] {
        assert_eq!(h.menu_reason(&vcx, id), None, "{id}: live");
    }
    h.draw(&mut vcx);
    assert!(
        vcx.debug_bounds("calibration-menu-7").is_some(),
        "the menu is painted"
    );

    // `j` steps to Run all (row 1); `enter` picks it: the menu closes and
    // the verb runs through dispatch.
    h.press(&mut vcx, "j");
    assert_eq!(
        h.dispatched().last().map(String::as_str),
        Some(geode_tile::motion::MENU_DOWN)
    );
    assert_eq!(h.menu_open(&vcx).map(|(_, hl)| hl), Some(Some(1)));
    h.press(&mut vcx, "enter");
    assert_eq!(
        h.dispatched().last().map(String::as_str),
        Some("calibration::menu_pick")
    );
    assert_eq!(h.menu_open(&vcx), None, "closed by the pick");
    assert_eq!(h.key_context(&vcx), normal());
    let reqs = h.requests();
    let runs = calibrates(&reqs);
    assert_eq!(runs.len(), 1, "{reqs:?}");
    assert_eq!(runs[0].action, RunAction::Run { from: None });
    assert!(runs[0].tag > tag);

    // `.` toggles; `escape` closes.
    h.press(&mut vcx, ".");
    assert!(h.menu_open(&vcx).is_some());
    h.press(&mut vcx, ".");
    assert_eq!(h.menu_open(&vcx), None);
    h.press(&mut vcx, ".");
    h.press(&mut vcx, "escape");
    assert_eq!(h.menu_open(&vcx), None);
    assert_eq!(
        h.dispatched().last().map(String::as_str),
        Some("calibration::cancel")
    );

    // The header's `⋯` opens the same menu; a press on `Connect…` (row 6;
    // rows count the separators) closes it and runs the verb, which on
    // parity (its one port filled) is the connect refusal as a notice.
    h.click(&mut vcx, "calibration-menu-button-7");
    assert!(h.menu_open(&vcx).is_some(), "the trigger opens it");
    h.click(&mut vcx, "calibration-menu-row-7-6");
    assert_eq!(h.menu_open(&vcx), None, "an enabled pick closes the menu");
    assert_eq!(
        h.notices(&vcx),
        vec!["parity has no open input".to_string()]
    );
    // `Open stage` (row 5) is the shell's action: a pick closes the menu
    // and asks the shell through the frame rather than the tile's own
    // dispatch, which does not carry it.
    h.click(&mut vcx, "calibration-menu-button-7");
    assert!(h.requested_actions(&mut vcx).is_empty());
    h.click(&mut vcx, "calibration-menu-row-7-5");
    assert_eq!(h.menu_open(&vcx), None, "an enabled pick closes the menu");
    assert_eq!(
        h.requested_actions(&mut vcx),
        vec![(TileId(Harness::TILE), ActionId(OPEN_STAGE.to_string()))]
    );
    h.press(&mut vcx, ".");
    h.press(&mut vcx, "escape");

    // The rows follow the results: a held publish disables its row with
    // the same words `p` would say.
    h.deliver(
        &mut vcx,
        Delivery::Calibration(run_outcome(KEY, runs[0].tag, vec![held("publish", HELD)])),
    );
    h.press(&mut vcx, ".");
    assert_eq!(
        h.menu_reason(&vcx, "calibration::publish"),
        Some(format!("publish is held: {HELD}"))
    );
    // The menu owns the keys: a normal-mode chord resolves to nothing
    // while it is up. A verb reaching the tile anyway (the palette)
    // closes the menu before it runs.
    let before = h.dispatched().len();
    h.press(&mut vcx, "shift-l");
    assert!(h.menu_open(&vcx).is_some(), "shift+l is not the menu's");
    assert_eq!(h.dispatched().len(), before, "and reached no binding");
    assert!(h.act(&mut vcx, "calibration::relayout"));
    assert_eq!(h.menu_open(&vcx), None, "another verb closes the menu");
    assert_eq!(h.key_context(&vcx), normal());
}

/// The `:` line: `run [stage]`, `publish`, `pipeline <name>` and
/// `layout`; each refusal is the line's answer, and a pipeline switch
/// clears the results, drops a cursor the new pipeline lacks and asks the
/// new sources.
#[gpui::test]
fn colon_commands_run_publish_and_switch_pipeline(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        Some(two_pipeline_config()),
        Some(restored(Some("SPX"), None, Some("parity"))),
    );
    h.load(&mut vcx, "SPX");
    assert_eq!(h.command(&mut vcx, "run parity"), Ok(()));
    let reqs = h.requests();
    assert_eq!(
        calibrates(&reqs)[0].action,
        RunAction::Run {
            from: Some("parity".into())
        }
    );
    assert_eq!(h.command(&mut vcx, "run"), Ok(()));
    let reqs = h.requests();
    assert_eq!(calibrates(&reqs)[0].action, RunAction::Run { from: None });
    assert_eq!(
        h.command(&mut vcx, "run nope"),
        Err("unknown stage 'nope'".to_string())
    );
    assert_eq!(
        h.command(&mut vcx, "run a b"),
        Err("usage: :run [stage]".to_string())
    );
    assert!(h.requests().is_empty());

    assert_eq!(h.command(&mut vcx, "publish"), Ok(()));
    assert_eq!(
        h.confirm_text(&vcx).as_deref(),
        Some("Publish repo, box for SPX to sophis? y/n")
    );
    h.press(&mut vcx, "n");
    assert_eq!(h.confirm_text(&vcx), None);
    assert!(h.requests().is_empty(), "n sends nothing");

    assert_eq!(
        h.command(&mut vcx, "pipeline nope"),
        Err("unknown pipeline 'nope'".to_string())
    );
    assert_eq!(
        h.command(&mut vcx, "pipeline"),
        Err("usage: :pipeline <name>".to_string())
    );
    assert_eq!(h.command(&mut vcx, "pipeline funding_only"), Ok(()));
    assert_eq!(h.counts(&vcx).as_deref(), Some("1 stages \u{b7} 0 fits"));
    assert!(h.result_names(&vcx).is_empty(), "the results are cleared");
    assert_eq!(h.cursor(&vcx), None, "parity is not a stage here");
    assert_eq!(
        h.serialize(&vcx),
        restored(Some("SPX"), Some("funding_only"), None)
    );
    let reqs = h.requests();
    let asked = docs(&reqs);
    assert_eq!(asked.len(), 1, "{reqs:?}");
    assert_eq!(asked[0].dataset, "rate_curve");
    assert!(calibrates(&reqs).is_empty());
    assert_eq!(h.footer(&vcx), "waiting for funding");
    // No publish stage here: the line says so.
    assert_eq!(
        h.command(&mut vcx, "publish"),
        Err("no publish stage in funding_only".to_string())
    );

    assert_eq!(
        h.command(&mut vcx, "bogus"),
        Err("unknown command 'bogus'".to_string())
    );
    assert_eq!(h.command(&mut vcx, "layout"), Ok(()));
}
