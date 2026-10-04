//! The footer notice's dismissal, through the production routes: the
//! notice reached through the data path, a press on the painted notice,
//! `escape` through the keymap.

use super::*;

const FAILED: &str = "cvi read failed";

impl Harness {
    /// Answer every outstanding document read: the CVI read with
    /// `cvi` (a document, or a failure), the chain read with `chains`.
    fn answer_reads(
        &self,
        vcx: &mut gpui::VisualTestContext,
        cvi: Result<&DocumentRows, &str>,
        chains: &[ChainExpiry],
    ) {
        loop {
            let reqs = self.requests();
            let asked: Vec<(String, u64)> = docs(&reqs)
                .iter()
                .map(|p| (p.dataset.clone(), p.tag))
                .collect();
            if asked.is_empty() {
                break;
            }
            for (dataset, tag) in asked {
                if dataset == CVI {
                    match cvi {
                        Ok(doc) => self.answer_doc(vcx, tag, cvi_snapshot(doc)),
                        Err(e) => self.deliver(
                            vcx,
                            Delivery::Query(QueryOutcome {
                                key: KEY,
                                tag,
                                snapshot: Err(e.into()),
                                submitted: Instant::now(),
                            }),
                        ),
                    }
                } else {
                    self.answer_doc(vcx, tag, chain_snapshot(chains));
                }
            }
        }
    }

    fn footer_painted(&self, vcx: &mut gpui::VisualTestContext) -> bool {
        self.draw(vcx);
        painted(vcx, &format!("volslice-notice-{TILE}"))
    }
}

/// Following A on SPX.Z, its CVI read failed: the footer's danger notice.
fn failed(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    let (h, mut vcx) = open_bound(cx, None);
    h.show(&mut vcx);
    h.follow_a(&mut vcx);
    h.post(&mut vcx, scope_of("SPX.Z"));
    let (_, chains) = published();
    h.answer_reads(&mut vcx, Err(FAILED), &chains);
    assert_eq!(h.footer_notice_text(&vcx).as_deref(), Some(FAILED));
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.footer_tone()),
        Some(geode_tile::notice::Tone::Danger)
    );
    assert!(h.footer_painted(&mut vcx), "fixture: the notice paints");
    (h, vcx)
}

impl Harness {
    fn footer_notice_text(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile
            .read_with(vcx, |t, _| t.footer_notice().map(|n| n.to_string()))
    }
}

/// A click hides the danger notice and runs no verb; the tile still
/// reports it, so a repaint keeps it hidden. A good read clears it, and
/// the failure coming back paints again.
#[gpui::test]
fn a_click_dismisses_the_footer_notice_until_it_stops_and_returns(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = failed(cx);
    let verbs = h.dispatched(&vcx).len();
    click(
        &mut vcx,
        &format!("volslice-notice-text-{TILE}"),
        Modifiers::none(),
    );
    assert!(!h.footer_painted(&mut vcx), "dismissed");
    assert!(
        h.notices(&vcx).contains(&FAILED.to_string()),
        "still reported"
    );
    assert_eq!(h.dispatched(&vcx).len(), verbs, "the press ran no verb");
    assert!(
        h.tile.read_with(&vcx, |t, _| t.menu_open()).is_none(),
        "and opened nothing"
    );

    vcx.simulate_keystrokes("j");
    assert!(!h.footer_painted(&mut vcx), "re-prepared unchanged: hidden");

    let (doc, chains) = published();
    h.widen_keeping_spx(&mut vcx, "B1");
    h.answer_reads(&mut vcx, Ok(&doc), &chains);
    assert!(!h.notices(&vcx).contains(&FAILED.to_string()), "it stopped");
    // An as-of move asks again; the read fails again.
    h.frame.update(&mut vcx, |f, cx| {
        f.shared_mut()
            .set_as_of(geode_core::query::AsOf::At(chrono::Utc::now()));
        cx.notify();
    });
    vcx.run_until_parked();
    h.answer_reads(&mut vcx, Err(FAILED), &chains);
    assert_eq!(h.footer_notice_text(&vcx).as_deref(), Some(FAILED));
    assert!(h.footer_painted(&mut vcx), "back: it shows again");
}

/// `escape` is last in line: an open menu closes first and a refusal
/// clears first, each leaving the notice; the next `escape` dismisses it.
#[gpui::test]
fn escape_dismisses_the_footer_notice_after_every_other_layer(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = failed(cx);
    vcx.simulate_keystrokes(".");
    assert!(
        h.tile.read_with(&vcx, |t, _| t.menu_open()).is_some(),
        "fixture: the menu is up"
    );
    vcx.simulate_keystrokes("escape");
    assert!(
        h.tile.read_with(&vcx, |t, _| t.menu_open()).is_none(),
        "the menu closed"
    );
    assert!(h.footer_painted(&mut vcx), "the notice stays");

    // Following, `u` is refused: the refusal leads the footer.
    vcx.simulate_keystrokes("u");
    assert_eq!(
        h.footer_notice_text(&vcx).as_deref(),
        Some("following A \u{2014} set the underlying there (+1 more)")
    );
    vcx.simulate_keystrokes("escape");
    assert_eq!(
        h.footer_notice_text(&vcx).as_deref(),
        Some(FAILED),
        "the refusal cleared"
    );
    assert!(h.footer_painted(&mut vcx), "the notice stays");

    vcx.simulate_keystrokes("escape");
    assert!(!h.footer_painted(&mut vcx), "dismissed");
    assert!(
        h.notices(&vcx).contains(&FAILED.to_string()),
        "still reported"
    );
}

/// An empty state (`no underlying`) is a status notice: neither a press
/// nor `escape` hides it.
#[gpui::test]
fn a_status_footer_notice_is_not_dismissed(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.show(&mut vcx);
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.footer_tone()),
        Some(geode_tile::notice::Tone::Status)
    );
    assert!(h.footer_painted(&mut vcx));
    click(
        &mut vcx,
        &format!("volslice-notice-text-{TILE}"),
        Modifiers::none(),
    );
    assert!(h.footer_painted(&mut vcx), "a press leaves it");
    vcx.simulate_keystrokes("escape");
    assert!(h.footer_painted(&mut vcx), "escape leaves it");
}
