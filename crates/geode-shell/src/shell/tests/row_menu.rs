//! The shell's row menu: `tile::context_menu` (`g .`) on the focused
//! tile's cursor row, its keys, its picks, and what closes it.

use super::launch::{draw, focused, underlying_state};
use super::*;
use crate::module::recording::{Recorded, RecordingAction, RecordingFactory};
use geode_core::context::DimensionContext;
use std::rc::Rc;

type Log = Rc<std::cell::RefCell<Vec<Recorded>>>;

const ROW_FRAGMENT: &str = "[[bindings]]\ncontext = \"rec\"\n[bindings.keys]\n\"g g\" = \"rec::noop\"\n\"g .\" = \"tile::context_menu\"\n\"g m\" = \"tile::open_with\"\n";

struct Fixture {
    services: ShellServices,
    log: Log,
    runs: Rc<std::cell::RefCell<Vec<DimensionContext>>>,
}

/// "rec" reports `context` and accepts underlying_ref; one action on
/// position_ref records its runs.
fn fixture(context: Option<DimensionContext>) -> Fixture {
    let mut rec = RecordingFactory::new("rec");
    rec.fragment = Some(ROW_FRAGMENT);
    rec.accepts = &["underlying_ref"];
    *rec.dimension_context.borrow_mut() = context;
    let log = rec.log.clone();
    let action = RecordingAction::new("test::nemo", "Open in Nemo", "position_ref");
    let runs = action.runs.clone();
    let mut services = services_with_recorders(vec![rec]);
    assert!(
        services.keymap_fragment_diagnostics.is_empty(),
        "{:?}",
        services.keymap_fragment_diagnostics
    );
    services.roster.add_action(Rc::new(action));
    Fixture {
        services,
        log,
        runs,
    }
}

fn row_menu_titles(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> Option<Vec<String>> {
    shell.read_with(cx, |s, _| s.row_menu.as_ref().map(|m| m.titles()))
}

fn spx_p7() -> DimensionContext {
    DimensionContext::of(&[("underlying_ref", "SPX"), ("position_ref", "P7")])
}

#[gpui::test]
fn g_dot_opens_the_row_menu_on_the_cursor_row(cx: &mut gpui::TestAppContext) {
    let f = fixture(Some(spx_p7()));
    let (window, mut vcx) = open_shell(cx, f.services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    vcx.simulate_keystrokes("g .");
    draw(&mut vcx);
    assert_eq!(
        row_menu_titles(&shell, &vcx),
        Some(vec![
            "# underlying_ref \u{b7} SPX".into(),
            "Open Rec".into(),
            "|".into(),
            "# position_ref \u{b7} P7".into(),
            "Open in Nemo".into(),
        ])
    );
    assert!(vcx.debug_bounds("row-menu").is_some(), "painted");
}

#[gpui::test]
fn enter_on_open_splits_a_tile_with_the_launch_state(cx: &mut gpui::TestAppContext) {
    let f = fixture(Some(spx_p7()));
    let (window, mut vcx) = open_shell(cx, f.services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let source = focused(&shell, &vcx);
    vcx.simulate_keystrokes("g .");
    draw(&mut vcx);
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert!(
        row_menu_titles(&shell, &vcx).is_none(),
        "a pick closes the menu"
    );
    let new = focused(&shell, &vcx);
    assert_ne!(new, source, "the pick split a new tile");
    let expected = underlying_state("SPX");
    assert!(
        f.log
            .borrow()
            .iter()
            .any(|r| matches!(r, Recorded::Created(t, Some(s)) if *t == new && *s == expected)),
        "{:?}",
        f.log.borrow()
    );
}

#[gpui::test]
fn j_then_enter_runs_the_action_with_the_context(cx: &mut gpui::TestAppContext) {
    let f = fixture(Some(spx_p7()));
    let (window, mut vcx) = open_shell(cx, f.services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    vcx.simulate_keystrokes("g . j enter");
    draw(&mut vcx);
    assert!(row_menu_titles(&shell, &vcx).is_none());
    assert_eq!(f.runs.borrow().as_slice(), &[spx_p7()]);
}

#[gpui::test]
fn escape_closes_and_consumes(cx: &mut gpui::TestAppContext) {
    let f = fixture(Some(spx_p7()));
    let (window, mut vcx) = open_shell(cx, f.services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    // `x` is unbound; `g g` is a bound bare sequence (`rec::noop`) whose
    // dispatch would close the menu if the keys leaked past it.
    vcx.simulate_keystrokes("g . x g g");
    draw(&mut vcx);
    assert!(
        row_menu_titles(&shell, &vcx).is_some(),
        "bare keys are consumed, the menu stays"
    );
    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);
    assert!(row_menu_titles(&shell, &vcx).is_none());
}

#[gpui::test]
fn a_chord_closes_the_row_menu_and_dispatches(cx: &mut gpui::TestAppContext) {
    let f = fixture(Some(spx_p7()));
    let (window, mut vcx) = open_shell(cx, f.services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let tiles_before = shell.read_with(&vcx, |s, _| {
        s.services.workspaces.active().tree().tiles().len()
    });
    vcx.simulate_keystrokes("g . ctrl-v");
    draw(&mut vcx);
    assert!(row_menu_titles(&shell, &vcx).is_none());
    let tiles_after = shell.read_with(&vcx, |s, _| {
        s.services.workspaces.active().tree().tiles().len()
    });
    assert_eq!(tiles_after, tiles_before + 1, "the chord still split");
}

#[gpui::test]
fn the_palette_closes_the_row_menu(cx: &mut gpui::TestAppContext) {
    let f = fixture(Some(spx_p7()));
    let (window, mut vcx) = open_shell(cx, f.services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    vcx.simulate_keystrokes("g .");
    draw(&mut vcx);
    assert!(row_menu_titles(&shell, &vcx).is_some());
    // The palette key, not `dispatch`: the key opens the palette ahead of
    // the matcher, so no dispatch preamble closes the menu for it.
    vcx.simulate_keystrokes("ctrl-k");
    draw(&mut vcx);
    assert!(
        shell.read_with(&vcx, |s, _| s.palette.is_some()),
        "palette open"
    );
    assert!(row_menu_titles(&shell, &vcx).is_none());
}

#[gpui::test]
fn g_dot_on_a_row_with_no_actions_says_so(cx: &mut gpui::TestAppContext) {
    for context in [None, Some(DimensionContext::of(&[("lhu", "7")]))] {
        let f = fixture(context);
        let (window, mut vcx) = open_shell(cx, f.services);
        let shell = shell_of(&window, &mut vcx);
        vcx.simulate_keystrokes("ctrl-v");
        draw(&mut vcx);
        vcx.simulate_keystrokes("g .");
        draw(&mut vcx);
        assert!(row_menu_titles(&shell, &vcx).is_none());
        assert_eq!(
            shell.read_with(&vcx, |s, _| s.notice),
            Some(crate::shell::row_menu::NO_ROW_ACTIONS)
        );
    }
}

/// Part 1 obligation: with an action column registered, `g m` on a row
/// holding only that column finds no kind and says so.
#[gpui::test]
fn g_m_on_a_row_with_only_an_action_column_shows_the_notice(cx: &mut gpui::TestAppContext) {
    let f = fixture(Some(DimensionContext::of(&[("position_ref", "P7")])));
    let (window, mut vcx) = open_shell(cx, f.services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    vcx.simulate_keystrokes("g m");
    draw(&mut vcx);
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.notice),
        Some(crate::shell::input::NO_MODULE_OPENS)
    );
}
