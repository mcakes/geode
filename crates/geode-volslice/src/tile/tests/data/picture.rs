//! What the painted picture belongs to: the header names the underlying
//! whose documents are on screen, never one still being asked about; and a
//! refusal or failure for another underlying clears the old picture rather
//! than leaving it under the new name or strip.

use super::*;

impl Harness {
    /// The underlying the prepared header names.
    fn header_underlying(&self, vcx: &gpui::VisualTestContext) -> String {
        self.tile
            .read_with(vcx, |t, _| t.header_underlying().to_string())
    }
    /// Whether the strip has rows.
    fn has_strip(&self, vcx: &gpui::VisualTestContext) -> bool {
        self.tile.read_with(vcx, |t, _| !t.strip.is_empty())
    }
}

/// SPX.Z loaded and painted, under its own underlying.
fn painted_spx(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    let (h, mut vcx) = open_on(cx, launched_on("SPX.Z"));
    h.show(&mut vcx);
    let (doc, chains) = published();
    let reqs = h.answer_documents(&mut vcx, &doc, &chains);
    let first = vols(&reqs)[0].clone();
    h.answer_vol(&mut vcx, &first);
    assert!(!h.labels(&vcx).is_empty());
    assert_eq!(h.header_underlying(&vcx), "SPX.Z");
    (h, vcx)
}

/// While NDX.Z's documents are asked for, SPX.Z's picture is the one on
/// screen and the header names it; the name moves with the documents.
#[gpui::test]
fn the_header_names_the_painted_underlying_while_another_is_asked(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = painted_spx(cx);
    h.command(&mut vcx, "underlying NDX.Z").unwrap();
    let reqs = h.requests();
    assert_eq!(docs(&reqs)[0].document_key, vec!["NDX.Z".to_string()]);
    assert_eq!(h.header_underlying(&vcx), "SPX.Z", "the curves are SPX.Z's");
    let (doc, chains) = published();
    let tag = docs(&reqs)[0].tag;
    h.answer_doc(&mut vcx, tag, cvi_snapshot(&doc));
    let _ = h.requests();
    h.answer_doc(&mut vcx, tag, chain_snapshot(&chains));
    assert_eq!(h.header_underlying(&vcx), "NDX.Z");
}

/// A refused CVI read for a new underlying leaves nothing of the old one
/// on screen: the header names what was asked, over no strip and no
/// curves, and the refusal is the notice.
#[gpui::test]
fn a_refused_read_for_a_new_underlying_clears_the_old_picture(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = painted_spx(cx);
    h.data.fill_for_tests();
    h.command(&mut vcx, "underlying NDX.Z").unwrap();
    let _ = h.requests();
    assert_eq!(
        h.notices(&vcx),
        vec!["document request refused: the data service is busy".to_string()]
    );
    assert_eq!(h.header_underlying(&vcx), "NDX.Z");
    assert!(!h.has_strip(&vcx), "no SPX.Z strip under NDX.Z");
    assert!(h.labels(&vcx).is_empty(), "no SPX.Z curves under NDX.Z");
}

/// A refused chain read fails the fetch for a new underlying: the old
/// picture clears the same way.
#[gpui::test]
fn a_failed_fetch_for_a_new_underlying_clears_the_old_picture(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = painted_spx(cx);
    h.command(&mut vcx, "underlying NDX.Z").unwrap();
    let reqs = h.requests();
    let tag = docs(&reqs)[0].tag;
    h.data.fill_for_tests();
    let (doc, _) = published();
    h.answer_doc(&mut vcx, tag, cvi_snapshot(&doc));
    let _ = h.requests();
    assert_eq!(
        h.notices(&vcx),
        vec!["document request refused: the data service is busy".to_string()]
    );
    assert_eq!(h.header_underlying(&vcx), "NDX.Z");
    assert!(!h.has_strip(&vcx));
    assert!(h.labels(&vcx).is_empty());
}

/// A failed fetch for the underlying already on screen keeps its picture:
/// the last good documents are still that underlying's.
#[gpui::test]
fn a_failed_fetch_for_the_same_underlying_keeps_the_picture(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = painted_spx(cx);
    let labels = h.labels(&vcx);
    // A publication moves the data counter the tile watches: it asks again.
    h.frame.update(&mut vcx, |f, cx| {
        f.note_published(geode_shell::frame::Publish {
            dataset: CVI.into(),
            batch: "SPX.Z".into(),
            books: 1,
            at: chrono::Utc::now(),
        });
        cx.notify();
    });
    vcx.run_until_parked();
    let reqs = h.requests();
    let tag = docs(&reqs)[0].tag;
    h.data.fill_for_tests();
    let (doc, _) = published();
    h.answer_doc(&mut vcx, tag, cvi_snapshot(&doc));
    let _ = h.requests();
    assert_eq!(h.header_underlying(&vcx), "SPX.Z");
    assert!(h.has_strip(&vcx));
    assert_eq!(h.labels(&vcx), labels, "the last good picture stays");
}

/// A vol batch refused right after a new underlying's documents installed
/// would leave the old curves under the new strip: the model clears, and
/// the new strip and header stand with the refusal. A refusal for the
/// underlying the curves belong to keeps them.
#[gpui::test]
fn a_refused_batch_after_a_new_install_clears_the_old_curves(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = painted_spx(cx);
    h.data.fill_for_tests();
    vcx.simulate_keystrokes("x");
    let _ = h.requests();
    assert!(
        h.notices(&vcx)
            .contains(&"vol request refused: the data service is busy".to_string())
    );
    assert!(
        !h.labels(&vcx).is_empty(),
        "same underlying: the curves stay"
    );

    h.command(&mut vcx, "underlying NDX.Z").unwrap();
    let reqs = h.requests();
    let tag = docs(&reqs)[0].tag;
    let (doc, chains) = published();
    h.answer_doc(&mut vcx, tag, cvi_snapshot(&doc));
    let _ = h.requests();
    h.data.fill_for_tests();
    h.answer_doc(&mut vcx, tag, chain_snapshot(&chains));
    let _ = h.requests();
    assert_eq!(
        h.notices(&vcx),
        vec!["vol request refused: the data service is busy".to_string()]
    );
    assert_eq!(h.header_underlying(&vcx), "NDX.Z");
    assert!(h.has_strip(&vcx), "NDX.Z's strip");
    assert!(h.labels(&vcx).is_empty(), "no SPX.Z curves under NDX.Z");
}
