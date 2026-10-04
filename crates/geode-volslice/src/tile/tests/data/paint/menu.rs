//! The `.` action menu and the differences axis's fixed y domain, through
//! the production routes: keys through the keymap, `:` lines through the
//! content door, presses on painted elements.

use super::*;
use crate::core::build::{DIFF_AXIS, restyled};
use crate::core::menu::{FIX_DIFF_Y, NO_DIFF_DOMAIN};
use geode_chart::core::scale::nice_outward;
use geode_shell::actions::ActionId;

/// A loaded tile showing `cvi − chain` in the lower pane at 2026-11-20,
/// an expiry the chain quotes, focused, its batch answered and painted.
fn with_diff(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    let (h, mut vcx) = loaded(cx);
    h.focus(&mut vcx);
    h.command(&mut vcx, "diff cvi - chain").unwrap();
    vcx.simulate_keystrokes("j space");
    assert_eq!(h.active(&vcx), ["2026-11-20"]);
    h.answer_last(&mut vcx).expect("the pair resubmits");
    h.draw(&mut vcx);
    assert!(
        h.tile.read_with(&vcx, |t, _| t
            .model()
            .slots
            .iter()
            .any(|s| s.axis == DIFF_AXIS)),
        "the pair paints"
    );
    (h, vcx)
}

impl Harness {
    fn menu(&self, vcx: &gpui::VisualTestContext) -> Option<(Vec<String>, Option<usize>)> {
        self.tile.read_with(vcx, |t, _| t.menu_open())
    }
    fn ylim(&self, vcx: &gpui::VisualTestContext) -> Option<(f64, f64)> {
        self.tile.read_with(vcx, |t, _| t.state().diff_ylim)
    }
    /// The differences axis's domain as the element scales it now.
    fn shown_diff_domain(&self, vcx: &gpui::VisualTestContext) -> Option<(f64, f64)> {
        self.tile.read_with(vcx, |t, _| t.diff_domain())
    }
    /// What the differences axis would autoscale to at the current view.
    fn auto_diff_domain(&self, vcx: &gpui::VisualTestContext) -> Option<(f64, f64)> {
        self.tile.read_with(vcx, |t, _| {
            restyled(t.model(), t.state().split, None, 0).side_domain(DIFF_AXIS, t.painted_view())
        })
    }
    fn footer(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile
            .read_with(vcx, |t, _| t.footer_notice().map(|n| n.to_string()))
    }
    /// The open menu's tick on `id`'s row: `None` with no menu.
    fn menu_tick(&self, vcx: &gpui::VisualTestContext, id: &str) -> Option<Option<bool>> {
        self.tile.read_with(vcx, |t, _| t.menu_tick(id))
    }
    fn highlighted_id(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        let (ids, at) = self.menu(vcx)?;
        // `menu_open` lists action rows only; map the row index back.
        let rows = self.tile.read_with(vcx, |t, cx| t.menu_rows(cx));
        let row = rows.get(at?)?.action()?.pick().0.clone();
        assert!(ids.contains(&row));
        Some(row)
    }
}

/// `.` opens the menu as a fieldless list: the shared `j`/`k` step its
/// enabled rows (the strip's cursor stays), and `enter` on `Fix diff
/// y-axis` freezes the lower axis at the domain it shows. A pan then
/// leaves that domain where it was, though autoscaling would move it.
#[gpui::test]
fn the_menu_fixes_the_diff_axis_at_the_shown_domain_and_a_pan_keeps_it(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = with_diff(cx);
    // Zoom in first, so a pan has somewhere to go.
    vcx.simulate_keystrokes("= =");
    let shown = h.shown_diff_domain(&vcx).expect("the pair paints");
    vcx.simulate_keystrokes(".");
    assert_eq!(h.mode(&vcx), "menu");
    assert!(h.tilelist(&vcx));
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t
            .key_context()
            .get("popup")
            .map(str::to_string)),
        Some("actions".to_string())
    );
    assert_eq!(
        h.highlighted_id(&vcx).as_deref(),
        Some("volslice::underlying")
    );
    let cursor = h.state(&vcx).cursor;
    // underlying → coordinate → cvi → cvi draft → chain → densities →
    // difference → fix.
    vcx.simulate_keystrokes("j j j j j j j");
    assert_eq!(h.highlighted_id(&vcx).as_deref(), Some(FIX_DIFF_Y));
    assert_eq!(h.state(&vcx).cursor, cursor, "the strip's j stays out");
    let hint = vcx.update(|window, cx| h.tile.read(cx).diff_tick_hint(window));
    let frozen = nice_outward(shown, hint);
    assert!(
        frozen.0 <= shown.0 && frozen.1 >= shown.1 && frozen != shown,
        "widened outward to tick values: {frozen:?} over {shown:?}"
    );
    vcx.simulate_keystrokes("enter");
    assert_eq!(h.menu(&vcx), None, "a pick closes the menu");
    assert_eq!(h.ylim(&vcx), Some(frozen));
    let shown = frozen;
    let log = h.dispatched(&vcx);
    assert!(
        log.ends_with(&["volslice::menu_pick".into(), FIX_DIFF_Y.into()]),
        "{log:?}"
    );
    let before = h.view(&vcx);
    vcx.simulate_keystrokes("l");
    assert_ne!(h.view(&vcx), before, "the view panned");
    assert_eq!(
        h.shown_diff_domain(&vcx),
        Some(shown),
        "the fixed domain stays"
    );
    assert_ne!(
        h.auto_diff_domain(&vcx),
        Some(shown),
        "autoscaled, the pan would have moved it"
    );
    // `0` resets x and keeps the fixed domain.
    vcx.simulate_keystrokes("0");
    assert_eq!(h.ylim(&vcx), Some(shown));
    assert_eq!(h.shown_diff_domain(&vcx), Some(shown));
}

/// `:ylim` sets the domain in the axis's units or in percents; the header
/// then shows the chip, whose click frees the axis as the action does.
#[gpui::test]
fn ylim_sets_the_domain_and_the_header_chip_frees_it(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = with_diff(cx);
    let version = h.version(&vcx);
    h.command(&mut vcx, "ylim -2% 2%").unwrap();
    assert_eq!(h.ylim(&vcx), Some((-0.02, 0.02)));
    assert_eq!(h.shown_diff_domain(&vcx), Some((-0.02, 0.02)));
    assert!(h.version(&vcx) > version, "a new model version");
    assert_eq!(
        h.command(&mut vcx, "ylim 0.02 -0.02"),
        Err("the lower limit must be below the upper".to_string())
    );
    assert_eq!(
        h.ylim(&vcx),
        Some((-0.02, 0.02)),
        "a refusal changes nothing"
    );
    h.draw(&mut vcx);
    let chip = format!("volslice-ylim-chip-{TILE}-y \u{2212}2%\u{2026}2%");
    click(&mut vcx, &chip, Modifiers::default());
    assert_eq!(h.ylim(&vcx), None);
    assert_eq!(
        h.dispatched(&vcx).last().map(String::as_str),
        Some(FIX_DIFF_Y)
    );
    h.draw(&mut vcx);
    assert!(!painted(&mut vcx, &chip), "autoscaled: no chip");
    h.command(&mut vcx, "ylim -0.01 0.03").unwrap();
    h.command(&mut vcx, "ylim off").unwrap();
    assert_eq!(h.ylim(&vcx), None);
}

/// The fixed domain is saved while set and restored into the first model.
#[gpui::test]
fn a_saved_ylim_restores_into_the_model(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = with_diff(cx);
    h.command(&mut vcx, "ylim -0.03 0.01").unwrap();
    let table = vcx.update(|_, cx| h.content.serialize(cx));
    let (h2, mut vcx2) = open_on(cx, table);
    h2.show(&mut vcx2);
    let (doc, chains) = published();
    let reqs = h2.answer_documents(&mut vcx2, &doc, &chains);
    h2.answer_vol(&mut vcx2, vols(&reqs)[0]);
    assert_eq!(h2.ylim(&vcx2), Some((-0.03, 0.01)));
    assert_eq!(
        h2.tile
            .read_with(&vcx2, |t, _| t.model().y_limit(DIFF_AXIS)),
        Some((-0.03, 0.01))
    );
}

/// With no difference shown there is no domain to freeze: the row says
/// why, and the action, reached from the palette, refuses with a notice.
#[gpui::test]
fn fixing_with_no_differences_refuses(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    h.focus(&mut vcx);
    vcx.simulate_keystrokes(".");
    let rows = h.tile.read_with(&vcx, |t, cx| t.menu_rows(cx));
    let fix = rows
        .iter()
        .filter_map(|r| r.action())
        .find(|a| a.pick().0 == FIX_DIFF_Y)
        .unwrap();
    assert_eq!(fix.reason().map(|r| r.as_ref()), Some(NO_DIFF_DOMAIN));
    vcx.simulate_keystrokes("escape");
    assert_eq!(h.menu(&vcx), None);
    vcx.update(|window, cx| {
        h.content
            .dispatch(&ActionId(FIX_DIFF_Y.into()), None, window, cx)
    });
    assert_eq!(h.ylim(&vcx), None);
    assert_eq!(h.footer(&vcx).as_deref(), Some(NO_DIFF_DOMAIN));
    // Shown a difference, the same action succeeds and the refusal is gone.
    h.command(&mut vcx, "diff cvi - chain").unwrap();
    assert_eq!(h.footer(&vcx), None, "a `:` line is a verb: the refusal clears");
    vcx.simulate_keystrokes("j space");
    h.answer_last(&mut vcx).expect("the pair resubmits");
    h.draw(&mut vcx);
    vcx.update(|window, cx| {
        h.content
            .dispatch(&ActionId(FIX_DIFF_Y.into()), None, window, cx)
    });
    assert!(h.ylim(&vcx).is_some());
    assert!(!h.notices(&vcx).contains(&NO_DIFF_DOMAIN.to_string()));
    assert_ne!(h.footer(&vcx).as_deref(), Some(NO_DIFF_DOMAIN));
}

/// A disabled row picked by key (the pointer lit it; `enter` picks the
/// lit row) or by a click gives its reason as the refusal and the menu
/// stays up.
#[gpui::test]
fn a_disabled_row_picked_by_key_or_click_says_why(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    h.focus(&mut vcx);
    vcx.simulate_keystrokes(".");
    h.draw(&mut vcx);
    let rows = h.tile.read_with(&vcx, |t, cx| t.menu_rows(cx));
    let fix = rows
        .iter()
        .position(|r| r.action().is_some_and(|a| a.pick().0 == FIX_DIFF_Y))
        .unwrap();
    let row = format!("volslice-menu-row-{TILE}-{fix}");
    let at = bounds(&mut vcx, &row).center();
    vcx.simulate_event(gpui::MouseMoveEvent {
        position: at,
        pressed_button: None,
        modifiers: Modifiers::default(),
    });
    assert_eq!(
        h.menu(&vcx).and_then(|m| m.1),
        Some(fix),
        "the pointer lit it"
    );
    vcx.simulate_keystrokes("enter");
    assert!(h.menu(&vcx).is_some(), "the menu stays");
    assert_eq!(h.footer(&vcx).as_deref(), Some(NO_DIFF_DOMAIN));
    assert_eq!(h.ylim(&vcx), None);
    vcx.simulate_keystrokes("escape");
    assert_eq!(h.footer(&vcx), None, "escape is a verb: the refusal clears");
    vcx.simulate_keystrokes(".");
    h.draw(&mut vcx);
    click(&mut vcx, &row, Modifiers::default());
    assert!(h.menu(&vcx).is_some());
    assert_eq!(h.footer(&vcx).as_deref(), Some(NO_DIFF_DOMAIN));
}

/// Rows follow the tile under an open menu: a `:ylim` ticks the fix row.
#[gpui::test]
fn an_open_menus_rows_follow_the_tile(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = with_diff(cx);
    vcx.simulate_keystrokes(".");
    assert_eq!(h.menu_tick(&vcx, FIX_DIFF_Y), Some(Some(false)));
    h.command(&mut vcx, "ylim -2% 2%").unwrap();
    assert!(h.menu(&vcx).is_some(), "a :ylim leaves the menu up");
    assert_eq!(h.menu_tick(&vcx, FIX_DIFF_Y), Some(Some(true)));
}

/// `.` in the diff chooser swaps the action menu in, as `\u{22ef}` does.
#[gpui::test]
fn dot_in_the_chooser_opens_the_action_menu(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    h.focus(&mut vcx);
    vcx.simulate_keystrokes("d");
    assert!(h.chooser(&vcx).is_some());
    vcx.simulate_keystrokes(".");
    assert!(h.chooser(&vcx).is_none());
    assert!(h.menu(&vcx).is_some());
}

/// `.` toggles; the `\u{22ef}` button toggles; a right press on the chart's
/// plot opens the menu on a focused tile and only focuses an unfocused
/// one; a pick of `Difference…` opens the chooser through `d`'s path.
#[gpui::test]
fn the_menu_opens_from_dot_the_button_and_a_chart_right_press(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = loaded(cx);
    let chart = format!("volslice-chart-{TILE}");
    right_click(&mut vcx, &chart);
    assert_eq!(h.menu(&vcx), None, "unfocused: it only focuses");
    h.focus(&mut vcx);
    right_click(&mut vcx, &chart);
    assert!(h.menu(&vcx).is_some(), "a right press on the plot opens it");
    vcx.simulate_keystrokes(".");
    assert_eq!(h.menu(&vcx), None, "`.` closes it");
    let button = format!("volslice-menu-button-{TILE}");
    h.draw(&mut vcx);
    click(&mut vcx, &button, Modifiers::default());
    assert!(h.menu(&vcx).is_some(), "the button opens it");
    h.draw(&mut vcx);
    click(&mut vcx, &button, Modifiers::default());
    assert_eq!(h.menu(&vcx), None, "and closes it");
    // A row click picks: `Difference…` opens the chooser.
    vcx.simulate_keystrokes(".");
    h.draw(&mut vcx);
    let rows = h.tile.read_with(&vcx, |t, cx| t.menu_rows(cx));
    let diff = rows
        .iter()
        .position(|r| r.action().is_some_and(|a| a.pick().0 == "volslice::diff"))
        .unwrap();
    click(
        &mut vcx,
        &format!("volslice-menu-row-{TILE}-{diff}"),
        Modifiers::default(),
    );
    assert_eq!(h.menu(&vcx), None);
    assert!(h.chooser(&vcx).is_some(), "the chooser is up");
    assert_eq!(h.mode(&vcx), "menu");
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t
            .key_context()
            .get("popup")
            .map(str::to_string)),
        Some("diff".to_string())
    );
    // The button over the chooser swaps the menu in: the chooser's
    // outside press, painted for the chooser, leaves the menu up.
    h.draw(&mut vcx);
    click(&mut vcx, &button, Modifiers::default());
    assert!(h.chooser(&vcx).is_none());
    assert!(h.menu(&vcx).is_some(), "the menu replaced the chooser");
}
