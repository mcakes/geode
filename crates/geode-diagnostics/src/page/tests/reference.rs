//! The Reference section through its production routes: keys through the
//! page's keymap, toolbar clicks, and the `Diagnostics` demand the bridge
//! drains.

use super::keys::key;
use super::*;
use geode_core::query::{AsOf, QueryKey, ReferenceOutcome, ReferenceTable};
use geode_shell::diagnostics::ReferenceLane;

/// A shown page whose `Diagnostics` declares `names`; the show's own
/// catalog demand is drained so later checks see only what they cause.
fn shown_with(h: &Harness, vcx: &mut gpui::VisualTestContext, names: &[&str]) {
    h.diagnostics.update(vcx, |d, cx| {
        d.set_reference_datasets(names.iter().map(|s| s.to_string()).collect());
        cx.notify();
    });
    h.page.update(vcx, |p, cx| p.set_visible(true, cx));
    h.diagnostics
        .update(vcx, |d, _| d.take_pending_catalog_request());
    focus_page(h, vcx);
}

fn table(rows: &[&str]) -> ReferenceTable {
    ReferenceTable {
        columns: vec!["underlying_ref".into(), "calendar".into()],
        rows: rows
            .iter()
            .map(|r| vec![Some(r.to_string()), Some("XNYS".into())])
            .collect(),
        gen_id: 3,
        source_time: chrono::DateTime::from_timestamp(0, 0).unwrap(),
    }
}

fn answer_at(
    h: &Harness,
    vcx: &mut gpui::VisualTestContext,
    dataset: &str,
    as_of: AsOf,
    rows: &[&str],
) {
    h.diagnostics.update(vcx, |d, cx| {
        d.set_reference(ReferenceOutcome {
            key: QueryKey(0),
            tag: 1,
            dataset: dataset.into(),
            as_of,
            table: Ok(Some(table(rows))),
        });
        cx.notify();
    });
    vcx.run_until_parked();
}

fn answer(h: &Harness, vcx: &mut gpui::VisualTestContext, dataset: &str, rows: &[&str]) {
    answer_at(h, vcx, dataset, AsOf::Live, rows);
}

fn take_read(h: &Harness, vcx: &mut gpui::VisualTestContext) -> Option<String> {
    h.diagnostics.update(vcx, |d, _| d.take_reference_request())
}

fn take_poll(h: &Harness, vcx: &mut gpui::VisualTestContext) -> Option<String> {
    h.diagnostics.update(vcx, |d, _| d.take_poll_request())
}

/// `/`, type into the filter as a keyboard would, then Enter back to
/// navigation, keeping the text.
fn type_filter(h: &Harness, vcx: &mut gpui::VisualTestContext, text: &str) {
    assert!(key(h, vcx, "/"));
    vcx.simulate_input(text);
    vcx.run_until_parked();
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
}

fn status(h: &Harness, vcx: &gpui::VisualTestContext) -> String {
    h.page
        .read_with(vcx, |p, _| p.reference_status.0.to_string())
}

fn summary(h: &Harness, vcx: &gpui::VisualTestContext) -> String {
    h.page.read_with(vcx, |p, _| p.result_summary.to_string())
}

fn badge(h: &Harness, vcx: &gpui::VisualTestContext) -> usize {
    h.page.read_with(vcx, |p, _| p.badges.reference)
}

fn empty_title(h: &Harness, vcx: &gpui::VisualTestContext) -> String {
    h.page.read_with(vcx, |p, cx| {
        p.table.read(cx).delegate().empty_title().to_string()
    })
}

#[gpui::test]
fn g_r_shows_the_reference_section_and_asks_for_its_table(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    shown_with(&h, &mut vcx, &["underlyings"]);
    assert!(key(&h, &mut vcx, "g r"));
    assert_eq!(
        h.page.read_with(&vcx, |p, _| p.section()),
        Section::Reference
    );
    assert_eq!(take_read(&h, &mut vcx), Some("underlyings".into()));
}

#[gpui::test]
fn reference_requests_only_while_the_section_is_shown(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    shown_with(&h, &mut vcx, &["underlyings"]);
    // On Sources, a reference publish asks nothing.
    h.diagnostics.update(&mut vcx, |d, cx| {
        d.note_published("underlyings");
        cx.notify();
    });
    assert_eq!(take_read(&h, &mut vcx), None);
    key(&h, &mut vcx, "g r");
    take_read(&h, &mut vcx);
    // On Reference, the publish asks again.
    h.diagnostics.update(&mut vcx, |d, cx| {
        d.note_published("underlyings");
        cx.notify();
    });
    assert_eq!(take_read(&h, &mut vcx), Some("underlyings".into()));
    // A publish of another dataset is not this table's.
    h.diagnostics.update(&mut vcx, |d, cx| {
        d.note_published("risk");
        cx.notify();
    });
    assert_eq!(take_read(&h, &mut vcx), None);
    // Hidden, nothing; shown again, the show asks.
    h.page.update(&mut vcx, |p, cx| p.set_visible(false, cx));
    h.diagnostics.update(&mut vcx, |d, cx| {
        d.note_published("underlyings");
        cx.notify();
    });
    assert_eq!(take_read(&h, &mut vcx), None);
    h.page.update(&mut vcx, |p, cx| p.set_visible(true, cx));
    assert_eq!(take_read(&h, &mut vcx), Some("underlyings".into()));
}

#[gpui::test]
fn an_as_of_change_on_reference_asks_again_and_reads_loading(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    shown_with(&h, &mut vcx, &["underlyings"]);
    key(&h, &mut vcx, "g r");
    answer(&h, &mut vcx, "underlyings", &["SPX"]);
    take_read(&h, &mut vcx);
    assert!(status(&h, &vcx).starts_with("gen 3 · "));
    set_frame_as_of(
        &h,
        &mut vcx,
        AsOf::At(chrono::DateTime::from_timestamp(100, 0).unwrap()),
    );
    assert_eq!(take_read(&h, &mut vcx), Some("underlyings".into()));
    assert_eq!(status(&h, &vcx), "Loading");
}

/// Re-asks are edge-triggered. A refused read bumps the reference counter
/// and rebuilds the section; were the rebuild to ask, the refusal would
/// ask again forever. A stale answer reads Loading without asking either.
#[gpui::test]
fn a_refused_read_does_not_ask_again(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    shown_with(&h, &mut vcx, &["underlyings"]);
    key(&h, &mut vcx, "g r");
    assert!(take_read(&h, &mut vcx).is_some());
    let rebuilds = h.page.read_with(&vcx, |p, _| p.rebuild_count);
    h.diagnostics.update(&mut vcx, |d, cx| {
        d.note_reference_refused(
            "underlyings",
            ReferenceLane::Read,
            "the data service is busy — press r to retry",
        );
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(h.page.read_with(&vcx, |p, _| p.rebuild_count) > rebuilds);
    assert_eq!(
        status(&h, &vcx),
        "the data service is busy — press r to retry"
    );
    assert_eq!(take_read(&h, &mut vcx), None, "a refusal asks nothing");
    // An answer for another as-of is stale: Loading, and still no ask.
    answer_at(
        &h,
        &mut vcx,
        "underlyings",
        AsOf::At(chrono::DateTime::from_timestamp(100, 0).unwrap()),
        &["SPX"],
    );
    assert_eq!(status(&h, &vcx), "Loading");
    assert_eq!(take_read(&h, &mut vcx), None, "a stale answer asks nothing");
}

#[gpui::test]
fn tab_cycles_reference_datasets(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    shown_with(&h, &mut vcx, &["underlyings", "calendars"]);
    key(&h, &mut vcx, "g r");
    take_read(&h, &mut vcx);
    assert!(key(&h, &mut vcx, "tab"));
    assert_eq!(h.page.read_with(&vcx, |p, _| p.reference_view), 1);
    assert_eq!(take_read(&h, &mut vcx), Some("calendars".into()));
    assert!(key(&h, &mut vcx, "tab"));
    assert_eq!(h.page.read_with(&vcx, |p, _| p.reference_view), 0, "wraps");
    assert_eq!(take_read(&h, &mut vcx), Some("underlyings".into()));
    assert!(key(&h, &mut vcx, "shift+tab"));
    assert_eq!(h.page.read_with(&vcx, |p, _| p.reference_view), 1);
    assert_eq!(take_read(&h, &mut vcx), Some("calendars".into()));
}

/// With one dataset there is nothing to step to: Tab neither moves nor
/// asks.
#[gpui::test]
fn tab_with_one_dataset_asks_nothing(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    shown_with(&h, &mut vcx, &["underlyings"]);
    key(&h, &mut vcx, "g r");
    take_read(&h, &mut vcx);
    assert!(key(&h, &mut vcx, "tab"));
    assert_eq!(h.page.read_with(&vcx, |p, _| p.reference_view), 0);
    assert_eq!(take_read(&h, &mut vcx), None);
}

/// `r` polls the dataset's sources and reads the table again: an
/// unchanged poll publishes nothing, so the read is what retries a
/// refused one.
#[gpui::test]
fn r_on_reference_asks_the_source_to_poll(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    shown_with(&h, &mut vcx, &["underlyings"]);
    key(&h, &mut vcx, "g r");
    take_read(&h, &mut vcx);
    assert!(key(&h, &mut vcx, "r"));
    assert_eq!(take_poll(&h, &mut vcx), Some("underlyings".into()));
    assert_eq!(take_read(&h, &mut vcx), Some("underlyings".into()));
    assert!(
        !h.diagnostics
            .update(&mut vcx, |d, _| d.take_pending_catalog_request()),
        "r here is not a catalog refresh"
    );
}

/// The toolbar's Poll now does what `r` does and hands focus back to the
/// page; a dataset button selects that dataset and asks for it.
#[gpui::test]
fn the_toolbar_polls_and_picks_datasets(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    shown_with(&h, &mut vcx, &["underlyings", "calendars"]);
    key(&h, &mut vcx, "g r");
    take_read(&h, &mut vcx);
    click(&mut vcx, "diagnostics-reference-poll");
    vcx.run_until_parked();
    assert_eq!(take_poll(&h, &mut vcx), Some("underlyings".into()));
    assert_eq!(take_read(&h, &mut vcx), Some("underlyings".into()));
    assert!(vcx.update(|window, cx| h.page.read(cx).focus_handle().is_focused(window)));
    click(&mut vcx, "diagnostics-reference-view-calendars");
    vcx.run_until_parked();
    assert_eq!(h.page.read_with(&vcx, |p, _| p.reference_view), 1);
    assert_eq!(take_read(&h, &mut vcx), Some("calendars".into()));
    assert!(vcx.update(|window, cx| h.page.read(cx).focus_handle().is_focused(window)));
}

#[gpui::test]
fn the_reference_filter_and_copy_work_on_rows(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    shown_with(&h, &mut vcx, &["underlyings"]);
    key(&h, &mut vcx, "g r");
    answer(&h, &mut vcx, "underlyings", &["SPX", "SX5E"]);
    assert_eq!(h.page.read_with(&vcx, |p, _| p.prepared.rows.len()), 2);
    assert!(key(&h, &mut vcx, "j"));
    assert!(key(&h, &mut vcx, "y"));
    let copied = vcx.read(|cx| cx.read_from_clipboard().unwrap().text().unwrap());
    assert_eq!(copied, "underlying_ref: SX5E\ncalendar: XNYS");
    type_filter(&h, &mut vcx, "spx");
    assert_eq!(h.page.read_with(&vcx, |p, _| p.prepared.rows.len()), 1);
    assert_eq!(summary(&h, &vcx), "1 of 2 rows");
    type_filter(&h, &mut vcx, "nothing");
    assert_eq!(empty_title(&h, &vcx), "Filter matches nothing (2 rows)");
}

/// A NULL cell copies as `—`, distinct from an empty string.
#[gpui::test]
fn a_null_cell_copies_as_a_dash(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    shown_with(&h, &mut vcx, &["underlyings"]);
    key(&h, &mut vcx, "g r");
    let mut with_null = table(&["SX5E"]);
    with_null.rows[0][1] = None;
    h.diagnostics.update(&mut vcx, |d, cx| {
        d.set_reference(ReferenceOutcome {
            key: QueryKey(0),
            tag: 1,
            dataset: "underlyings".into(),
            as_of: AsOf::Live,
            table: Ok(Some(with_null)),
        });
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(key(&h, &mut vcx, "y"));
    let copied = vcx.read(|cx| cx.read_from_clipboard().unwrap().text().unwrap());
    assert_eq!(copied, "underlying_ref: SX5E\ncalendar: —");
}

/// The summary's total and the rail badge count the answer the table
/// shows. A stale-as-of answer for the same dataset stays on screen while
/// Loading, counted as what it is; the badge waits for the current answer.
#[gpui::test]
fn counts_come_from_the_answer_the_table_shows(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    shown_with(&h, &mut vcx, &["underlyings", "calendars"]);
    key(&h, &mut vcx, "g r");
    answer(&h, &mut vcx, "underlyings", &["SPX", "SX5E"]);
    assert_eq!(summary(&h, &vcx), "2 of 2 rows");
    assert_eq!(badge(&h, &vcx), 2);

    let at = AsOf::At(chrono::DateTime::from_timestamp(100, 0).unwrap());
    set_frame_as_of(&h, &mut vcx, at.clone());
    assert_eq!(status(&h, &vcx), "Loading");
    assert_eq!(h.page.read_with(&vcx, |p, _| p.prepared.rows.len()), 2);
    assert_eq!(summary(&h, &vcx), "2 of 2 rows", "the stale rows, counted");
    assert_eq!(badge(&h, &vcx), 0, "no badge while Loading");

    answer_at(&h, &mut vcx, "underlyings", at, &["SPX"]);
    assert_eq!(summary(&h, &vcx), "1 of 1 rows");
    assert_eq!(badge(&h, &vcx), 1);

    // Another dataset's answer is not shown or counted under this one.
    assert!(key(&h, &mut vcx, "tab"));
    assert_eq!(h.page.read_with(&vcx, |p, _| p.prepared.rows.len()), 0);
    assert_eq!(summary(&h, &vcx), "0 of 0 rows");
    assert_eq!(badge(&h, &vcx), 0);
    assert_eq!(empty_title(&h, &vcx), "Loading");
}

#[gpui::test]
fn no_declared_dataset_says_so(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    shown_with(&h, &mut vcx, &[]);
    key(&h, &mut vcx, "g r");
    assert_eq!(take_read(&h, &mut vcx), None);
    assert_eq!(status(&h, &vcx), "No reference datasets declared");
    assert_eq!(empty_title(&h, &vcx), "No reference datasets declared");
    assert!(key(&h, &mut vcx, "r"));
    assert_eq!(take_poll(&h, &mut vcx), None, "nothing to poll");
}

#[gpui::test]
fn the_reference_section_name_persists(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    focus_page(&h, &mut vcx);
    key(&h, &mut vcx, "g r");
    let saved = h.page.read_with(&vcx, |p, _| p.serialize());
    assert_eq!(
        saved.get("section").and_then(|v| v.as_str()),
        Some("reference")
    );
}

/// A source's health change reaches the shown chip without a new answer:
/// the worker degrades between publishes, and the chip must say so then.
#[gpui::test]
fn a_source_degrading_rewrites_the_shown_status(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    shown_with(&h, &mut vcx, &["underlyings"]);
    key(&h, &mut vcx, "g r");
    answer(&h, &mut vcx, "underlyings", &["SPX"]);
    assert!(!status(&h, &vcx).contains("refused"));
    h.diagnostics.update(&mut vcx, |d, cx| {
        let mut summary = geode_shell::diagnostics::SourceSummary::for_dataset("underlyings");
        summary.shape = geode_shell::diagnostics::SourceShape::Snapshot;
        d.describe_source("refdb", summary);
        d.note_health(
            "refdb",
            geode_core::health::Health::Degraded {
                reason: "connection refused".into(),
            },
            String::new(),
            std::time::SystemTime::UNIX_EPOCH,
        );
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(
        status(&h, &vcx).ends_with(" · connection refused"),
        "{}",
        status(&h, &vcx)
    );
}
