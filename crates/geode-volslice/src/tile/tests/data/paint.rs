//! The painted surface: the header, the strip, the chart's pointer
//! gestures and the footer. Presses, wheels and drags are simulated events
//! on painted elements; what is painted is read through `debug_selector`s.

use super::*;
use crate::core::build::{min_span, padded};
use crate::core::model::{Kind, Pair};
use geode_chart::core::Rect;
use geode_core::vol::Coordinate;
use gpui::{Modifiers, MouseButton, Pixels, Point, point, px};

fn loaded(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    let (h, mut vcx) = open_on(cx, launched_on("SPX.Z"));
    h.show(&mut vcx);
    let (doc, chains) = published();
    let reqs = h.answer_documents(&mut vcx, &doc, &chains);
    let first = vols(&reqs)[0].clone();
    h.answer_vol(&mut vcx, &first);
    h.draw(&mut vcx);
    (h, vcx)
}

impl Harness {
    fn active(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.tile.read_with(vcx, |t, _| {
            t.state()
                .active
                .iter()
                .flatten()
                .map(|e| e.to_string())
                .collect()
        })
    }
    /// The shell's word that this tile is focused, given from its render.
    fn focus(&self, vcx: &mut gpui::VisualTestContext) {
        self.in_draw(vcx, None, Some(true));
        self.draw(vcx);
    }
    fn view(&self, vcx: &gpui::VisualTestContext) -> View {
        self.tile.read_with(vcx, |t, _| t.view()).expect("a view")
    }
    /// The upper plot's rect in window space, from the painted surface.
    fn plot(&self, vcx: &mut gpui::VisualTestContext) -> Rect {
        let b = bounds(vcx, &format!("volslice-chart-{TILE}"));
        let rem = vcx.update(|w, _| w.rem_size().as_f32());
        let options = self
            .tile
            .read_with(vcx, |t, _| t.model().layout_options(rem));
        let rect = Rect::new(0.0, 0.0, b.size.width.as_f32(), b.size.height.as_f32());
        let p = geode_chart::Layout::solve(rect, options).upper.plot;
        Rect::new(
            p.x + b.origin.x.as_f32(),
            p.y + b.origin.y.as_f32(),
            p.w,
            p.h,
        )
    }
    /// The x value painted under window x `x`.
    fn value_at(&self, vcx: &mut gpui::VisualTestContext, x: f32) -> f64 {
        let plot = self.plot(vcx);
        let view = self.view(vcx);
        let scale = self.tile.read_with(vcx, |t, _| t.model().x.scale());
        scale.value_at(x, view, plot)
    }
}

fn bounds(vcx: &mut gpui::VisualTestContext, selector: &str) -> gpui::Bounds<Pixels> {
    let s: &'static str = Box::leak(selector.to_string().into_boxed_str());
    vcx.debug_bounds(s)
        .unwrap_or_else(|| panic!("{selector} is painted"))
}

fn painted(vcx: &mut gpui::VisualTestContext, selector: &str) -> bool {
    let s: &'static str = Box::leak(selector.to_string().into_boxed_str());
    vcx.debug_bounds(s).is_some()
}

fn press(vcx: &mut gpui::VisualTestContext, at: Point<Pixels>, modifiers: Modifiers) {
    vcx.simulate_event(gpui::MouseDownEvent {
        position: at,
        modifiers,
        button: MouseButton::Left,
        click_count: 1,
        first_mouse: false,
    });
}

fn release(vcx: &mut gpui::VisualTestContext, at: Point<Pixels>) {
    vcx.simulate_event(gpui::MouseUpEvent {
        position: at,
        modifiers: Modifiers::default(),
        button: MouseButton::Left,
        click_count: 1,
    });
}

fn click(vcx: &mut gpui::VisualTestContext, selector: &str, modifiers: Modifiers) {
    let at = bounds(vcx, selector).center();
    press(vcx, at, modifiers);
    release(vcx, at);
    vcx.run_until_parked();
}

fn wheel(vcx: &mut gpui::VisualTestContext, at: Point<Pixels>, dx: f32, dy: f32) {
    vcx.simulate_event(gpui::ScrollWheelEvent {
        position: at,
        delta: gpui::ScrollDelta::Pixels(point(px(dx), px(dy))),
        modifiers: Modifiers::default(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    vcx.run_until_parked();
}

fn row(expiry: &str) -> String {
    format!("volslice-strip-{TILE}-{expiry}")
}

/// A press that only focuses the tile changes nothing; focused, a plain
/// press solos the row and a ctrl press toggles it.
#[gpui::test]
fn a_click_on_a_strip_row_solos_and_ctrl_click_toggles_when_focused(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    click(&mut vcx, &row("2026-12-18"), Modifiers::default());
    assert_eq!(h.active(&vcx), ["2026-10-16"], "unfocused: it only focuses");
    assert!(h.requests().is_empty());
    h.focus(&mut vcx);
    click(&mut vcx, &row("2026-12-18"), Modifiers::default());
    assert_eq!(h.active(&vcx), ["2026-12-18"]);
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.state().cursor), 2);
    h.answer_last(&mut vcx).expect("a solo resubmits");
    assert_eq!(h.labels(&vcx), vec!["cvi 2026-12-18".to_string()]);
    let ctrl = Modifiers {
        control: true,
        ..Modifiers::default()
    };
    click(&mut vcx, &row("2026-10-16"), ctrl);
    assert_eq!(h.active(&vcx), ["2026-10-16", "2026-12-18"]);
    assert!(h.answer_last(&mut vcx).is_some());
}

#[gpui::test]
fn a_kind_chip_click_toggles_the_kind(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    click(
        &mut vcx,
        &format!("volslice-kind-{TILE}-chain-chain"),
        Modifiers::default(),
    );
    assert_eq!(h.dispatched(&vcx), ["volslice::kind_3"]);
    assert!(
        h.tile
            .read_with(&vcx, |t, _| t.state().hidden.contains(&Kind::Chain))
    );
    // The diff chip opens the chooser, as `d` does.
    click(
        &mut vcx,
        &format!("volslice-diff-chip-{TILE}-diff"),
        Modifiers::default(),
    );
    assert_eq!(h.dispatched(&vcx)[1], "volslice::diff");
    assert!(h.tile.read_with(&vcx, |t, _| t.chooser_rows().is_some()));
}

/// On the reversed delta axis a wheel zoom keeps the value under the
/// pointer where it is.
#[gpui::test]
fn the_wheel_zooms_about_the_pointer_on_a_reversed_axis_too(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    vcx.simulate_keystrokes("x x");
    h.answer_last(&mut vcx).unwrap();
    h.draw(&mut vcx);
    assert!(h.tile.read_with(&vcx, |t, _| t.model().x.reversed));
    let plot = h.plot(&mut vcx);
    let at = point(px(plot.x + plot.w * 0.3), px(plot.y + plot.h * 0.5));
    let before = h.value_at(&mut vcx, at.x.as_f32());
    let span = h.view(&vcx).span();
    wheel(&mut vcx, at, 0.0, 48.0);
    h.draw(&mut vcx);
    assert!(h.view(&vcx).span() < span, "zoomed in");
    let after = h.value_at(&mut vcx, at.x.as_f32());
    assert!(
        (after - before).abs() < 1e-9 * span.max(1.0),
        "{before} stays under the pointer, not {after}"
    );
}

/// A drag pans so that the value under the pointer follows it, on a
/// reversed axis as on a forward one.
#[gpui::test]
fn a_drag_pans_against_the_axis_direction(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    vcx.simulate_keystrokes("x x");
    h.answer_last(&mut vcx).unwrap();
    vcx.simulate_keystrokes("= = =");
    h.draw(&mut vcx);
    let plot = h.plot(&mut vcx);
    let from = point(px(plot.x + plot.w * 0.5), px(plot.y + plot.h * 0.5));
    let to = point(from.x + px(30.0), from.y);
    let grabbed = h.value_at(&mut vcx, from.x.as_f32());
    let before = h.view(&vcx);
    press(&mut vcx, from, Modifiers::default());
    h.draw(&mut vcx);
    assert!(matches!(
        h.tile.read_with(&vcx, |t, _| t.drag()),
        Some(Drag::Pan { .. })
    ));
    vcx.simulate_mouse_move(to, MouseButton::Left, Modifiers::default());
    h.draw(&mut vcx);
    let after = h.view(&vcx);
    assert!(
        (after.span() - before.span()).abs() < 1e-12,
        "a pan keeps the span"
    );
    assert_ne!(after.lo, before.lo, "it moved");
    let under = h.value_at(&mut vcx, to.x.as_f32());
    assert!(
        // Pixel positions are f32: a hair of rounding, against a wrong
        // sign's error of twice the drag's share of the span.
        (under - grabbed).abs() < 1e-6 * before.span(),
        "the grabbed value followed the pointer: {grabbed} vs {under}"
    );
    release(&mut vcx, to);
    h.draw(&mut vcx);
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.drag()), None);
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.state().view),
        Some((after.lo, after.hi)),
        "the saved view follows"
    );
}

#[gpui::test]
fn a_divider_drag_sets_the_split_as_a_new_version(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    // The chain's expiry, with the chain minus the cvi beneath it.
    vcx.simulate_keystrokes("j enter");
    let _ = h.requests();
    h.command(&mut vcx, "diff chain - cvi").unwrap();
    h.answer_last(&mut vcx).unwrap();
    h.draw(&mut vcx);
    let labels = h.labels(&vcx);
    assert!(
        labels.iter().any(|l| l.contains('\u{2212}')),
        "a difference slot: {labels:?}"
    );
    let divider = format!("volslice-divider-{TILE}");
    assert!(painted(&mut vcx, &divider), "two panes: a divider");
    let (v0, s0) = h
        .tile
        .read_with(&vcx, |t, _| (t.model().version, t.model().split));
    let band = bounds(&mut vcx, &divider).center();
    press(&mut vcx, band, Modifiers::default());
    h.draw(&mut vcx);
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.drag()), Some(Drag::Split));
    let to = point(band.x, band.y + px(30.0));
    vcx.simulate_mouse_move(to, MouseButton::Left, Modifiers::default());
    h.draw(&mut vcx);
    let (v1, s1) = h
        .tile
        .read_with(&vcx, |t, _| (t.model().version, t.model().split));
    assert!(
        s1 > s0,
        "dragged down: a taller upper pane ({s0} \u{2192} {s1})"
    );
    assert!(v1 > v0, "a new version");
    assert!(
        ((s1 * 100.0).round() / 100.0 - s1).abs() < 1e-6,
        "quantised: {s1}"
    );
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.state().split), s1);
    assert!(h.requests().is_empty(), "a split asks nothing");
    // A buttonless move is the release that was missed.
    vcx.simulate_mouse_move(to, None, Modifiers::default());
    h.draw(&mut vcx);
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.drag()), None);
}

/// A coordinate change resets a zoomed view to the new coordinate's padded
/// extent, with that coordinate's narrowest span.
#[gpui::test]
fn x_resets_the_view_to_the_new_extent(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    vcx.simulate_keystrokes("= =");
    let zoomed = h.view(&vcx);
    vcx.simulate_keystrokes("x");
    h.answer_last(&mut vcx).unwrap();
    let view = h.view(&vcx);
    let (doc, chains) = published();
    let loaded = Loaded {
        cvi: Some(doc),
        draft: None,
        chain: chains,
    };
    let narrowest = min_span(Coordinate::LogMoneyness, &loaded);
    assert_eq!(view.min_span, narrowest);
    let full = h.tile.read_with(&vcx, |t, _| t.model().full());
    let padded = padded(full, narrowest);
    assert_eq!((view.lo, view.hi), padded, "not {zoomed:?}");
}

#[gpui::test]
fn the_header_shows_the_link_chip_and_the_draft_mark(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_bound(cx, None);
    h.show(&mut vcx);
    h.draw(&mut vcx);
    assert!(painted(&mut vcx, &format!("volslice-empty-{TILE}")));
    h.follow_a(&mut vcx);
    h.post(&mut vcx, scope_of("SPX.Z"));
    let (doc, chains) = published();
    h.answer_documents(&mut vcx, &doc, &chains);
    let draft = cvi(&TERMS, Some(("2026-10-16", 0.01)));
    h.post(&mut vcx, draft_of("SPX.Z", &draft, DraftMark::Behind));
    h.draw(&mut vcx);
    assert!(painted(&mut vcx, &format!("tile-link-{TILE}-A-follow")));
    assert!(painted(&mut vcx, &format!("volslice-header-{TILE}")));
    assert!(painted(
        &mut vcx,
        &format!("volslice-kind-{TILE}-cvi draft-cvi draft \u{00b7} behind")
    ));
    assert!(painted(&mut vcx, &format!("volslice-kind-{TILE}-cvi-cvi")));
    assert!(painted(
        &mut vcx,
        &format!("volslice-kind-{TILE}-chain-chain")
    ));
    assert!(painted(&mut vcx, &format!("volslice-strip-{TILE}")));
    assert!(painted(&mut vcx, &row("2026-11-20")));
    h.command(&mut vcx, "diff cvi draft - cvi").unwrap();
    h.draw(&mut vcx);
    let label = Pair::new(Kind::Draft, Kind::Cvi).unwrap().label();
    assert!(painted(
        &mut vcx,
        &format!("volslice-diff-chip-{TILE}-diff: {label}")
    ));
}

/// The footer's notice line: the first notice and how many stand behind it.
#[gpui::test]
fn the_footer_paints_the_first_notice_and_counts_the_rest(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.show(&mut vcx);
    h.draw(&mut vcx);
    assert!(painted(&mut vcx, &format!("volslice-notice-{TILE}")));
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.footer_notice()),
        Some("no underlying".into())
    );
    let (h2, mut vcx2) = open_bound(cx, None);
    h2.show(&mut vcx2);
    h2.follow_a(&mut vcx2);
    vcx2.simulate_keystrokes("u");
    h2.draw(&mut vcx2);
    assert_eq!(
        h2.tile.read_with(&vcx2, |t, _| t.footer_notice()),
        Some("no underlying in A (+1 more)".into())
    );
}

/// Two draws with nothing changed: no path, no chrome and no prepared text
/// is rebuilt by the second.
#[gpui::test]
fn render_formats_nothing_per_frame(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    h.focus(&mut vcx);
    h.draw(&mut vcx);
    let paths = geode_chart::rebuilds();
    let chrome = geode_chart::chrome_rebuilds();
    let builds = h.tile.read_with(&vcx, |t, _| t.chrome_builds());
    h.draw(&mut vcx);
    h.draw(&mut vcx);
    assert_eq!(geode_chart::rebuilds(), paths);
    assert_eq!(geode_chart::chrome_rebuilds(), chrome);
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.chrome_builds()), builds);
    // A cursor step rebuilds no chart and formats no strip text.
    vcx.simulate_keystrokes("j");
    h.draw(&mut vcx);
    assert_eq!(geode_chart::rebuilds(), paths);
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.chrome_builds()), builds);
}

/// Showing runs inside the shell's draw, where the tile's notify is dropped
/// and its self-observer never hears it. A hidden tile with no underlying,
/// shown that way, still paints the notice the show produced, with no
/// further notify.
#[gpui::test]
fn a_tile_shown_from_the_shells_draw_paints_its_chrome_at_once(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.draw(&mut vcx);
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.footer_notice()), None);
    h.in_draw(&mut vcx, Some(true), None);
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.footer_notice()),
        Some("no underlying".into()),
        "prepared by the show itself"
    );
    assert!(painted(&mut vcx, &format!("volslice-notice-{TILE}")));
}

/// The done state, headless: a CVI panel stand-in emits a draft into A
/// and the viewer follows A. The front expiry paints the published curve
/// solid, its chain as points and the draft dashed; `d` on
/// `cvi draft - chain` adds the lower pane; two ctrl+clicks add two more
/// expiries in two more colors; `x x` reaches the reversed delta axis and
/// `shift+d` puts densities on the right axis. All of it is one painted
/// model, reached through the keymap, the strip's presses and the shell's
/// doors.
#[gpui::test]
fn the_done_state_in_one_frame(cx: &mut gpui::TestAppContext) {
    use geode_chart::Axis;
    use geode_chart::xy::{SlotKind, Style, XFormat};

    let (h, mut vcx) = open_bound(cx, None);
    h.show(&mut vcx);
    h.follow_a(&mut vcx);
    let draft = cvi(&TERMS, Some(("2026-10-16", 0.01)));
    h.post(&mut vcx, draft_of("SPX.Z", &draft, DraftMark::Editing));
    let (doc, _) = published();
    let chains = [
        chain("2026-10-16"),
        chain("2026-11-20"),
        chain("2027-06-18"),
    ];
    let reqs = h.answer_documents(&mut vcx, &doc, &chains);
    let first = vols(&reqs).last().map(|p| (*p).clone()).expect("a batch");
    assert_eq!(first.documents.len(), 2, "the cvi and the board's draft");
    h.answer_vol(&mut vcx, &first);
    let slot = |h: &Harness, vcx: &gpui::VisualTestContext, label: &str| {
        h.tile.read_with(vcx, |t, _| {
            t.model()
                .slots
                .iter()
                .find(|s| s.label.as_ref() == label)
                .cloned()
                .unwrap_or_else(|| panic!("{label} is painted"))
        })
    };
    assert_eq!(slot(&h, &vcx, "cvi 2026-10-16").style, Style::Solid);
    assert_eq!(slot(&h, &vcx, "cvi draft 2026-10-16").style, Style::Dashed);
    assert!(matches!(
        slot(&h, &vcx, "chain 2026-10-16").kind,
        SlotKind::Points { .. }
    ));

    // `d`, then the chooser's fifth row: none, cvi - cvi draft,
    // cvi - chain, cvi draft - cvi, cvi draft - chain.
    vcx.simulate_keystrokes("d j j j j enter");
    assert_eq!(h.state(&vcx).diff, Pair::new(Kind::Draft, Kind::Chain));
    h.answer_last(&mut vcx).expect("the pair resubmits");

    h.focus(&mut vcx);
    let ctrl = Modifiers {
        control: true,
        ..Modifiers::default()
    };
    click(&mut vcx, &row("2026-11-20"), ctrl);
    click(&mut vcx, &row("2026-12-18"), ctrl);
    assert_eq!(h.active(&vcx), ["2026-10-16", "2026-11-20", "2026-12-18"]);
    h.answer_last(&mut vcx).expect("the toggles resubmit");

    vcx.simulate_keystrokes("x x");
    h.answer_last(&mut vcx).expect("the coordinate resubmits");
    vcx.simulate_keystrokes("shift-d");
    h.answer_last(&mut vcx).expect("densities resubmit");
    h.draw(&mut vcx);

    let model = h.tile.read_with(&vcx, |t, _| t.model().clone());
    assert!(model.x.reversed && model.x.format == XFormat::Delta);
    assert_eq!(slot(&h, &vcx, "cvi 2026-10-16").style, Style::Solid);
    assert_eq!(slot(&h, &vcx, "cvi draft 2026-10-16").style, Style::Dashed);
    assert!(matches!(
        slot(&h, &vcx, "chain 2026-10-16").kind,
        SlotKind::Points { .. }
    ));
    let label = Pair::new(Kind::Draft, Kind::Chain).unwrap().label();
    let lower = slot(&h, &vcx, &format!("{label} 2026-10-16"));
    assert_eq!(lower.axis, Axis::BottomLeft, "the difference's own pane");
    assert!(painted(&mut vcx, &format!("volslice-divider-{TILE}")));
    let colors: Vec<_> = ["2026-10-16", "2026-11-20", "2026-12-18"]
        .iter()
        .map(|e| slot(&h, &vcx, &format!("cvi {e}")).color)
        .collect();
    assert!(
        colors[0] != colors[1] && colors[1] != colors[2] && colors[0] != colors[2],
        "three expiries, three colors: {colors:?}"
    );
    assert!(
        model
            .slots
            .iter()
            .any(|s| s.axis == Axis::Right && s.label.contains("density")),
        "a right-axis density slot"
    );
    assert!(h.notices(&vcx).is_empty(), "{:?}", h.notices(&vcx));
}
