//! The shell's row menu: `tile::context_menu` (`g .`) on the focused
//! tile's cursor row, its keys, its picks, and what closes it.

use super::drag::main_tile_point;
use super::launch::{draw, focused, underlying_state};
use super::*;
use crate::module::recording::{Recorded, RecordingAction, RecordingFactory};
use geode_core::colour::Tone;
use geode_core::context::DimensionContext;
use std::rc::Rc;

type Log = Rc<std::cell::RefCell<Vec<Recorded>>>;

const ROW_FRAGMENT: &str = "[[bindings]]\ncontext = \"rec\"\n[bindings.keys]\n\"g g\" = \"rec::noop\"\n\"g .\" = \"tile::context_menu\"\n\"g m\" = \"tile::open_with\"\n\"ctrl+alt+x ctrl+alt+y\" = \"rec::noop\"\n";

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
            shell.read_with(&vcx, |s, _| s.notice.clone()).as_deref(),
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
        shell.read_with(&vcx, |s, _| s.notice.clone()).as_deref(),
        Some(crate::shell::input::NO_MODULE_OPENS)
    );
}

#[gpui::test]
fn a_left_click_on_a_row_picks_it(cx: &mut gpui::TestAppContext) {
    let f = fixture(Some(spx_p7()));
    let (window, mut vcx) = open_shell(cx, f.services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    vcx.simulate_keystrokes("g .");
    draw(&mut vcx);
    // Rows: section, Open Rec, separator, section, Open in Nemo.
    assert_eq!(
        row_menu_titles(&shell, &vcx).map(|t| t[4].clone()),
        Some("Open in Nemo".to_string())
    );
    let row = vcx
        .debug_bounds("row-menu-row-4")
        .expect("the Nemo row paints");
    vcx.simulate_click(row.center(), gpui::Modifiers::none());
    draw(&mut vcx);
    assert!(row_menu_titles(&shell, &vcx).is_none());
    assert_eq!(f.runs.borrow().as_slice(), &[spx_p7()]);
}

/// Opened while the scope bar's text field held focus (the palette opened
/// from the field, or a user chord bound in the workspace context), the
/// menu hands focus back to the field when it closes itself.
#[gpui::test]
fn escape_returns_focus_to_the_filter_field_it_opened_from(cx: &mut gpui::TestAppContext) {
    let f = fixture(Some(spx_p7()));
    let (window, mut vcx) = open_shell(cx, f.services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let field = shell.read_with(&vcx, |s, cx| s.filter_input.read(cx).focus_handle(cx));
    vcx.update(|window, cx| field.focus(window, cx));
    draw(&mut vcx);
    assert!(vcx.update(|window, _| field.is_focused(window)));
    shell.update_in(&mut vcx, |s, window, cx| {
        s.open_row_menu(spx_p7(), None, false, window, cx)
    });
    draw(&mut vcx);
    assert!(row_menu_titles(&shell, &vcx).is_some());
    assert!(
        !vcx.update(|window, _| field.is_focused(window)),
        "the menu took the field's keys"
    );
    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);
    assert!(row_menu_titles(&shell, &vcx).is_none());
    assert!(vcx.update(|window, _| field.is_focused(window)));
}

/// A menu with no point to hang from (no recorded point, no focused tile)
/// would paint nothing yet swallow every key: the frame drops it.
#[gpui::test]
fn a_row_menu_with_nowhere_to_hang_is_dropped(cx: &mut gpui::TestAppContext) {
    let f = fixture(Some(spx_p7()));
    let (window, mut vcx) = open_shell(cx, f.services);
    let shell = shell_of(&window, &mut vcx);
    draw(&mut vcx);
    assert!(
        shell.read_with(&vcx, |s, _| s
            .services
            .workspaces
            .active()
            .focused_tile()
            .is_none()),
        "a fresh session has no tile"
    );
    shell.update_in(&mut vcx, |s, window, cx| {
        s.open_row_menu(spx_p7(), None, false, window, cx)
    });
    draw(&mut vcx);
    assert!(row_menu_titles(&shell, &vcx).is_none());
    // Keys reach the shell again: `ctrl-v` splits.
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    assert!(shell.read_with(&vcx, |s, _| {
        s.services.workspaces.active().focused_tile().is_some()
    }));
}

#[gpui::test]
fn a_right_press_opens_the_row_menu_at_the_pointer(cx: &mut gpui::TestAppContext) {
    let mut rec = RecordingFactory::new("rec");
    rec.accepts = &["underlying_ref"];
    *rec.press_context.borrow_mut() = Some(spx_p7());
    let services = services_with_recorders(vec![rec]);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let id = focused(&shell, &vcx);
    let at = main_tile_point(&mut vcx, &shell, id, 0.5, 0.5);
    vcx.simulate_mouse_down(at, gpui::MouseButton::Right, gpui::Modifiers::none());
    draw(&mut vcx);
    let opened_at = shell.read_with(&vcx, |s, _| s.row_menu.as_ref().and_then(|m| m.at()));
    assert_eq!(opened_at, Some(at));
    let tiles = shell.read_with(&vcx, |s, _| s.occupants.len());
    // A mouse-opened surface must take typed keys (grouping-picker rule).
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert!(shell.read_with(&vcx, |s, _| s.row_menu.is_none()));
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.occupants.len()),
        tiles + 1,
        "enter picked Open Rec"
    );
}

fn spx_own() -> DimensionContext {
    let mut ctx = DimensionContext::of(&[("underlying_ref", "SPX")]);
    ctx.own = Some("underlying_ref".into());
    ctx
}

#[gpui::test]
fn g_dot_offers_color_for_the_rows_own_text_dimension(cx: &mut gpui::TestAppContext) {
    let mut rec = RecordingFactory::new("rec");
    rec.fragment = Some(ROW_FRAGMENT);
    *rec.dimension_context.borrow_mut() = Some(spx_own());
    let (window, mut vcx) = open_shell(cx, services_with_recorders(vec![rec]));
    let shell = shell_of(&window, &mut vcx);
    shell.update(&mut vcx, |s, _| {
        s.text_dims.insert("underlying_ref".into());
    });
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    vcx.simulate_keystrokes("g .");
    draw(&mut vcx);
    assert_eq!(
        row_menu_titles(&shell, &vcx),
        Some(vec![
            "# underlying_ref \u{b7} SPX".into(),
            "Color\u{2026}".into(),
        ])
    );
    // Rows paint under index selectors: section, then Color….
    assert!(vcx.debug_bounds("row-menu-row-1").is_some(), "painted");
}

#[gpui::test]
fn a_right_press_offers_color_for_the_pressed_row(cx: &mut gpui::TestAppContext) {
    let rec = RecordingFactory::new("rec");
    *rec.press_context.borrow_mut() = Some(spx_own());
    let (window, mut vcx) = open_shell(cx, services_with_recorders(vec![rec]));
    let shell = shell_of(&window, &mut vcx);
    shell.update(&mut vcx, |s, _| {
        s.text_dims.insert("underlying_ref".into());
    });
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let id = focused(&shell, &vcx);
    let at = main_tile_point(&mut vcx, &shell, id, 0.5, 0.5);
    vcx.simulate_mouse_down(at, gpui::MouseButton::Right, gpui::Modifiers::none());
    draw(&mut vcx);
    assert_eq!(
        row_menu_titles(&shell, &vcx),
        Some(vec![
            "# underlying_ref \u{b7} SPX".into(),
            "Color\u{2026}".into(),
        ])
    );
}

/// An occupant that stops a right press's propagation (gpui-component's
/// selectable table does, on a cell) cannot hide it from the shell: the
/// tile cell's listener runs in the capture phase, so the press still
/// focuses the tile and opens the row menu.
#[gpui::test]
fn a_right_press_is_seen_when_the_occupant_stops_propagation(cx: &mut gpui::TestAppContext) {
    let mut rec = RecordingFactory::new("rec");
    rec.accepts = &["underlying_ref"];
    rec.stops_right_press = true;
    // Recorded by the occupant's own listener on the press: nothing until then.
    rec.pressed_context = Some(spx_p7());
    let services = services_with_recorders(vec![rec]);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let first = focused(&shell, &vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let second = focused(&shell, &vcx);
    assert_ne!(first, second, "the split focused the new tile");
    let at = main_tile_point(&mut vcx, &shell, first, 0.5, 0.5);
    vcx.simulate_mouse_down(at, gpui::MouseButton::Right, gpui::Modifiers::none());
    draw(&mut vcx);
    assert_eq!(focused(&shell, &vcx), first, "the press focused its tile");
    let opened_at = shell.read_with(&vcx, |s, _| s.row_menu.as_ref().and_then(|m| m.at()));
    assert_eq!(opened_at, Some(at));
    let tiles = shell.read_with(&vcx, |s, _| s.occupants.len());
    // A mouse-opened surface must take typed keys (grouping-picker rule).
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert!(shell.read_with(&vcx, |s, _| s.row_menu.is_none()));
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.occupants.len()),
        tiles + 1,
        "enter picked Open Rec"
    );
}

/// The captured listener sees every button; only a right press opens the
/// menu.
#[gpui::test]
fn a_left_press_never_opens_the_row_menu(cx: &mut gpui::TestAppContext) {
    let mut rec = RecordingFactory::new("rec");
    rec.accepts = &["underlying_ref"];
    *rec.press_context.borrow_mut() = Some(spx_p7());
    let services = services_with_recorders(vec![rec]);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let id = focused(&shell, &vcx);
    let at = main_tile_point(&mut vcx, &shell, id, 0.5, 0.5);
    vcx.simulate_mouse_down(at, gpui::MouseButton::Left, gpui::Modifiers::none());
    vcx.simulate_mouse_up(at, gpui::MouseButton::Left, gpui::Modifiers::none());
    draw(&mut vcx);
    assert!(shell.read_with(&vcx, |s, _| s.row_menu.is_none()));
    vcx.simulate_keystrokes("ctrl-{"); // tile → left dock
    draw(&mut vcx);
    let at = super::drag::dock_tile_point(&mut vcx, &shell, DockSide::Left, id, 0.5, 0.5);
    vcx.simulate_mouse_down(at, gpui::MouseButton::Left, gpui::Modifiers::none());
    vcx.simulate_mouse_up(at, gpui::MouseButton::Left, gpui::Modifiers::none());
    draw(&mut vcx);
    assert!(shell.read_with(&vcx, |s, _| s.row_menu.is_none()));
}

/// The dock's right-press listener is captured too.
#[gpui::test]
fn a_right_press_on_a_docked_tile_is_seen_when_the_occupant_stops_propagation(
    cx: &mut gpui::TestAppContext,
) {
    let mut rec = RecordingFactory::new("rec");
    rec.accepts = &["underlying_ref"];
    rec.stops_right_press = true;
    // Recorded by the occupant's own listener on the press: nothing until then.
    rec.pressed_context = Some(spx_p7());
    let services = services_with_recorders(vec![rec]);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let id = focused(&shell, &vcx);
    vcx.simulate_keystrokes("ctrl-{"); // tile → left dock
    draw(&mut vcx);
    let at = super::drag::dock_tile_point(&mut vcx, &shell, DockSide::Left, id, 0.5, 0.5);
    vcx.simulate_mouse_down(at, gpui::MouseButton::Right, gpui::Modifiers::none());
    draw(&mut vcx);
    let opened_at = shell.read_with(&vcx, |s, _| s.row_menu.as_ref().and_then(|m| m.at()));
    assert_eq!(opened_at, Some(at));
    let tiles = shell.read_with(&vcx, |s, _| s.occupants.len());
    // A mouse-opened surface must take typed keys (grouping-picker rule).
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert!(shell.read_with(&vcx, |s, _| s.row_menu.is_none()));
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.occupants.len()),
        tiles + 1,
        "enter picked Open Rec"
    );
}

/// A right press moves focus to the tile, so the menu it opens hands focus
/// home when it closes, never back to the scope bar's text field — even
/// when the field still held focus at the beat the menu opened (an occupant
/// that takes no focus on a press). The press's own beat is called directly:
/// a real press on the recording tile blurs the field through its
/// `track_focus`, which would hide the case.
#[gpui::test]
fn a_right_pressed_menu_never_returns_focus_to_the_filter_field(cx: &mut gpui::TestAppContext) {
    let mut rec = RecordingFactory::new("rec");
    rec.accepts = &["underlying_ref"];
    *rec.press_context.borrow_mut() = Some(spx_p7());
    let services = services_with_recorders(vec![rec]);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let id = focused(&shell, &vcx);
    let at = main_tile_point(&mut vcx, &shell, id, 0.5, 0.5);
    let field = shell.read_with(&vcx, |s, cx| s.filter_input.read(cx).focus_handle(cx));
    vcx.update(|window, cx| field.focus(window, cx));
    draw(&mut vcx);
    assert!(vcx.update(|window, _| field.is_focused(window)));
    shell.update_in(&mut vcx, |s, window, cx| {
        s.open_row_menu_from_press(id, at, window, cx)
    });
    draw(&mut vcx);
    assert!(row_menu_titles(&shell, &vcx).is_some());
    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);
    assert!(row_menu_titles(&shell, &vcx).is_none());
    assert!(
        !vcx.update(|window, _| field.is_focused(window)),
        "the press moved focus to the tile"
    );
}

#[gpui::test]
fn a_right_press_on_a_tile_without_press_context_opens_nothing(cx: &mut gpui::TestAppContext) {
    let mut rec = RecordingFactory::new("rec");
    rec.accepts = &["underlying_ref"];
    *rec.dimension_context.borrow_mut() = Some(spx_p7()); // g . would work
    let services = services_with_recorders(vec![rec]);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let id = focused(&shell, &vcx);
    let at = main_tile_point(&mut vcx, &shell, id, 0.5, 0.5);
    vcx.simulate_mouse_down(at, gpui::MouseButton::Right, gpui::Modifiers::none());
    draw(&mut vcx);
    assert!(shell.read_with(&vcx, |s, _| s.row_menu.is_none()));
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.notice.clone()).as_deref(),
        None,
        "no notice on a press either"
    );
}

#[gpui::test]
fn a_press_outside_the_row_menu_closes_it(cx: &mut gpui::TestAppContext) {
    let f = fixture(Some(spx_p7()));
    let (window, mut vcx) = open_shell(cx, f.services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    vcx.simulate_keystrokes("g .");
    draw(&mut vcx);
    assert!(row_menu_titles(&shell, &vcx).is_some());
    let far = gpui::point(gpui::px(5.), gpui::px(5.));
    vcx.simulate_mouse_down(far, gpui::MouseButton::Left, gpui::Modifiers::none());
    draw(&mut vcx);
    assert!(shell.read_with(&vcx, |s, _| s.row_menu.is_none()));
}

/// The dock's right-press listener opens the menu the same way.
#[gpui::test]
fn a_right_press_on_a_docked_tile_opens_the_row_menu_at_the_pointer(cx: &mut gpui::TestAppContext) {
    let mut rec = RecordingFactory::new("rec");
    rec.accepts = &["underlying_ref"];
    *rec.press_context.borrow_mut() = Some(spx_p7());
    let services = services_with_recorders(vec![rec]);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let id = focused(&shell, &vcx);
    vcx.simulate_keystrokes("ctrl-{"); // tile → left dock
    draw(&mut vcx);
    let at = super::drag::dock_tile_point(&mut vcx, &shell, DockSide::Left, id, 0.5, 0.5);
    vcx.simulate_mouse_down(at, gpui::MouseButton::Right, gpui::Modifiers::none());
    draw(&mut vcx);
    let opened_at = shell.read_with(&vcx, |s, _| s.row_menu.as_ref().and_then(|m| m.at()));
    assert_eq!(opened_at, Some(at));
    let tiles = shell.read_with(&vcx, |s, _| s.occupants.len());
    // A mouse-opened surface must take typed keys (grouping-picker rule).
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert!(shell.read_with(&vcx, |s, _| s.row_menu.is_none()));
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.occupants.len()),
        tiles + 1,
        "enter picked Open Rec"
    );
}

/// A chord prefix typed while the menu is open passes to the matcher; a
/// pick cancels it, so the sequence cannot complete after the menu.
#[gpui::test]
fn a_pick_cancels_a_chord_prefix_typed_while_open(cx: &mut gpui::TestAppContext) {
    let f = fixture(Some(spx_p7()));
    let (window, mut vcx) = open_shell(cx, f.services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let noops = || {
        f.log
            .borrow()
            .iter()
            .filter(|r| matches!(r, Recorded::Dispatch(_, a, _) if a.0 == "rec::noop"))
            .count()
    };
    // Sanity: the sequence dispatches on its own.
    vcx.simulate_keystrokes("ctrl-alt-x ctrl-alt-y");
    draw(&mut vcx);
    assert_eq!(noops(), 1);
    vcx.simulate_keystrokes("g . ctrl-alt-x");
    draw(&mut vcx);
    assert!(
        row_menu_titles(&shell, &vcx).is_some(),
        "a prefix is no dispatch"
    );
    let row = vcx
        .debug_bounds("row-menu-row-4")
        .expect("the Nemo row paints");
    vcx.simulate_click(row.center(), gpui::Modifiers::none());
    draw(&mut vcx);
    assert_eq!(f.runs.borrow().len(), 1, "the click picked Nemo");
    vcx.simulate_keystrokes("ctrl-alt-y");
    draw(&mut vcx);
    assert_eq!(noops(), 1, "the prefix did not survive the pick");
}

/// Opens `nemo://test/{value}` for its column and reports it, as a real
/// action would.
struct OpeningAction;

impl crate::dimension::DimensionAction for OpeningAction {
    fn id(&self) -> &'static str {
        "test::open"
    }
    fn title(&self) -> gpui::SharedString {
        "Open elsewhere".into()
    }
    fn column(&self) -> &'static str {
        "position_ref"
    }
    fn run(&self, ctx: &DimensionContext, acx: &mut crate::shell::row_menu::ActionCx<'_, '_>) {
        let url = format!(
            "nemo://test/{}",
            ctx.get("position_ref").unwrap_or_default()
        );
        acx.open_url(&url);
        acx.notice(format!("opened {url}"));
    }
}

fn opening_fixture(context: DimensionContext) -> ShellServices {
    let mut rec = RecordingFactory::new("rec");
    rec.fragment = Some(ROW_FRAGMENT);
    *rec.dimension_context.borrow_mut() = Some(context);
    let mut services = services_with_recorders(vec![rec]);
    services.roster.add_action(Rc::new(OpeningAction));
    services
}

#[gpui::test]
fn an_action_opens_its_url_through_the_opener(cx: &mut gpui::TestAppContext) {
    let opened: Rc<std::cell::RefCell<Vec<String>>> = Rc::default();
    let sink = opened.clone();
    cx.update(|cx| {
        cx.set_global(crate::dimension::UrlOpener(Rc::new(move |url, _| {
            sink.borrow_mut().push(url.to_string())
        })))
    });
    let (window, mut vcx) = open_shell(
        cx,
        opening_fixture(DimensionContext::of(&[("position_ref", "P7")])),
    );
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    vcx.simulate_keystrokes("g . enter");
    draw(&mut vcx);
    assert_eq!(opened.borrow().as_slice(), &["nemo://test/P7".to_string()]);
    assert_eq!(
        cx.opened_url(),
        None,
        "the opener took it; the OS never saw it"
    );
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.notice.clone()).as_deref(),
        Some("opened nemo://test/P7")
    );
}

#[gpui::test]
fn without_an_opener_the_app_opens_the_url(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(
        cx,
        opening_fixture(DimensionContext::of(&[("position_ref", "P7")])),
    );
    let _shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    vcx.simulate_keystrokes("g . enter");
    draw(&mut vcx);
    assert_eq!(cx.opened_url().as_deref(), Some("nemo://test/P7"));
}

// ---------------------------------------------------------------------
// Actions that choose a column value, then confirm.
// ---------------------------------------------------------------------

use crate::shell::{ACTION_KEY, choicedialog::Target};
use geode_core::query::{AsOf, DistinctOutcome, DistinctParams};
use geode_core::scope::Scope;

type Confirmed = Rc<std::cell::RefCell<Vec<String>>>;

/// Chooses an `lhu` value other than the row's own, then confirms it; a
/// yes records the value and says so.
struct MovingAction {
    confirmed: Confirmed,
}

impl crate::dimension::DimensionAction for MovingAction {
    fn id(&self) -> &'static str {
        "test::move"
    }
    fn title(&self) -> gpui::SharedString {
        "Move to LHU\u{2026}".into()
    }
    fn column(&self) -> &'static str {
        "position_ref"
    }
    fn run(&self, ctx: &DimensionContext, acx: &mut crate::shell::row_menu::ActionCx<'_, '_>) {
        acx.choose_value(
            ctx.clone(),
            "lhu",
            "Move to LHU".into(),
            ctx.get("lhu").map(str::to_string),
            "no LHU values to move to",
        );
    }
    fn chosen(
        &self,
        _ctx: &DimensionContext,
        value: &str,
        acx: &mut crate::shell::row_menu::ActionCx<'_, '_>,
    ) {
        let v = value.to_string();
        let confirmed = self.confirmed.clone();
        acx.confirm(
            format!("Move to LHU {value}?").into(),
            Rc::new(move |acx| {
                confirmed.borrow_mut().push(v.clone());
                acx.notice(format!("confirmed {v}"));
            }),
        );
    }
}

struct Moving {
    shell: Entity<ShellView>,
    vcx: gpui::VisualTestContext,
    requested: Rc<std::cell::RefCell<Vec<DistinctParams>>>,
    confirmed: Confirmed,
}

/// A shell on a row `{position_ref: P7, lhu: L1}` with [`MovingAction`]
/// registered, after `g . enter` ran it.
fn moving(cx: &mut gpui::TestAppContext) -> Moving {
    let mut m = moving_unopened(cx);
    m.vcx.simulate_keystrokes("g . enter");
    draw(&mut m.vcx);
    m
}

/// [`moving`] before anything opened the row menu.
fn moving_unopened(cx: &mut gpui::TestAppContext) -> Moving {
    let mut rec = RecordingFactory::new("rec");
    rec.fragment = Some(ROW_FRAGMENT);
    *rec.dimension_context.borrow_mut() = Some(DimensionContext::of(&[
        ("position_ref", "P7"),
        ("lhu", "L1"),
    ]));
    let mut services = services_with_recorders(vec![rec]);
    let confirmed: Confirmed = Rc::default();
    services.roster.add_action(Rc::new(MovingAction {
        confirmed: confirmed.clone(),
    }));
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let requested = Rc::new(std::cell::RefCell::new(Vec::new()));
    vcx.update(|_, cx| {
        let requested = requested.clone();
        cx.subscribe(&shell, move |_, e: &ShellEvent, _| {
            if let ShellEvent::DistinctRequested(p) = e {
                requested.borrow_mut().push(p.clone());
            }
        })
        .detach();
    });
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    Moving {
        shell,
        vcx,
        requested,
        confirmed,
    }
}

impl Moving {
    fn target(&self) -> Option<Target> {
        self.shell
            .read_with(&self.vcx, |s, _| s.choice_dialog_target())
    }
    fn tag(&self) -> u64 {
        match self.target() {
            Some(Target::ActionValue { tag, .. }) => tag,
            other => panic!("no action value dialog: {other:?}"),
        }
    }
    fn rows(&self) -> Option<Vec<String>> {
        self.shell.read_with(&self.vcx, |s, _| {
            s.choice_dialog.as_ref().map(|d| d.list.options().to_vec())
        })
    }
    fn notice(&self) -> Option<String> {
        self.shell
            .read_with(&self.vcx, |s, _| s.notice.as_ref().map(|n| n.to_string()))
    }
    fn depth(&self) -> usize {
        self.shell.read_with(&self.vcx, |s, _| s.modal_depth())
    }
    fn deliver(&mut self, tag: u64, values: Result<Vec<(&str, u64)>, String>) {
        let values = values.map(|v| v.into_iter().map(|(s, n)| (s.to_string(), n)).collect());
        self.shell.update(&mut self.vcx, |s, cx| {
            s.deliver_distinct(
                DistinctOutcome {
                    key: ACTION_KEY,
                    tag,
                    column: "lhu".into(),
                    values,
                },
                cx,
            )
        });
        draw(&mut self.vcx);
    }
    fn deliver_three(&mut self) {
        let tag = self.tag();
        self.deliver(tag, Ok(vec![("L1", 3), ("L2", 1), ("L3", 2)]));
    }
}

#[gpui::test]
fn choose_value_asks_for_the_columns_values_unscoped(cx: &mut gpui::TestAppContext) {
    let mut m = moving(cx);
    let requested = m.requested.borrow().clone();
    assert_eq!(requested.len(), 1, "{requested:?}");
    let req = &requested[0];
    assert_eq!(req.key, ACTION_KEY);
    assert_eq!(req.column, "lhu");
    assert_eq!(req.scope, Scope::default());
    assert_eq!(req.as_of, AsOf::Live);
    assert_eq!(req.tag, m.tag(), "the dialog waits on the request's tag");
    assert!(
        matches!(m.target(), Some(Target::ActionValue { values: None, .. })),
        "loading"
    );
    assert!(
        m.vcx.debug_bounds("action-loading").is_some(),
        "says loading"
    );
    assert_eq!(
        m.shell
            .read_with(&m.vcx, |s, _| s.top_modal().map(|t| t.title.clone())),
        Some("Move to LHU".into())
    );
}

#[gpui::test]
fn delivered_values_fill_the_dialog_without_the_excluded_one(cx: &mut gpui::TestAppContext) {
    let mut m = moving(cx);
    m.deliver_three();
    assert_eq!(m.rows(), Some(vec!["L2".to_string(), "L3".to_string()]));
    assert!(matches!(
        m.target(),
        Some(Target::ActionValue { values: Some(ref v), .. }) if v == &["L2", "L3"]
    ));
    assert!(m.vcx.debug_bounds("action-loading").is_none());
    assert!(m.vcx.debug_bounds("action-choice-L2").is_some(), "painted");
}

#[gpui::test]
fn a_stale_value_delivery_is_dropped(cx: &mut gpui::TestAppContext) {
    let mut m = moving(cx);
    let tag = m.tag();
    let before = m.target();
    m.deliver(tag + 1, Ok(vec![("L2", 1)]));
    assert_eq!(m.target(), before);
    assert_eq!(m.rows(), Some(vec![]));
    assert_eq!(m.depth(), 1, "still open");
}

#[gpui::test]
fn an_empty_value_list_says_so(cx: &mut gpui::TestAppContext) {
    let mut m = moving(cx);
    let tag = m.tag();
    m.deliver(tag, Ok(vec![("L1", 3)]));
    assert_eq!(m.depth(), 0, "closed");
    assert_eq!(m.notice().as_deref(), Some("no LHU values to move to"));
}

#[gpui::test]
fn a_failed_value_fetch_says_so(cx: &mut gpui::TestAppContext) {
    let mut m = moving(cx);
    let tag = m.tag();
    m.deliver(tag, Err("boom".into()));
    assert_eq!(m.depth(), 0, "closed");
    assert_eq!(
        m.notice().as_deref(),
        Some("could not load lhu values: boom")
    );
}

impl Moving {
    /// Push a plain dialog over whatever is open.
    fn cover(&mut self) {
        let shell = self.shell.clone();
        self.vcx.update(|window, cx| {
            shell.update(cx, |s, cx| {
                crate::shell::dialog::open_shell_dialog_with_key(
                    s,
                    window,
                    cx,
                    crate::shell::dialog::DialogKind::Plain,
                    "Cover",
                    |_, _, _| gpui::div().into_any_element(),
                    None,
                    false,
                )
            })
        });
        draw(&mut self.vcx);
    }
    /// Close the top dialog.
    fn close_top(&mut self) {
        let shell = self.shell.clone();
        self.vcx
            .update(|window, cx| shell.update(cx, |s, cx| s.close_modal(window, cx)));
        draw(&mut self.vcx);
    }
}

/// While loading there is nothing to choose: the footer offers only the
/// way out, and `enter` changes nothing.
#[gpui::test]
fn enter_while_loading_does_nothing(cx: &mut gpui::TestAppContext) {
    let mut m = moving(cx);
    assert!(m.vcx.debug_bounds("action-loading-hints").is_some());
    assert!(m.vcx.debug_bounds("action-hints").is_none());
    let before = m.target();
    m.vcx.simulate_keystrokes("enter");
    draw(&mut m.vcx);
    assert_eq!(m.depth(), 1, "still open");
    assert_eq!(m.target(), before, "still loading");
    assert!(m.confirmed.borrow().is_empty());
    assert_eq!(m.notice(), None);
    m.deliver_three();
    assert!(
        m.vcx.debug_bounds("action-hints").is_some(),
        "the full footer"
    );
    assert!(m.vcx.debug_bounds("action-loading-hints").is_none());
}

/// A confirm asked while a plain dialog is on top is refused with a
/// notice, not silently.
#[gpui::test]
fn a_confirm_under_a_plain_dialog_is_refused_with_a_notice(cx: &mut gpui::TestAppContext) {
    let mut m = moving_unopened(cx);
    m.cover();
    let shell = m.shell.clone();
    m.vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            let at = s
                .services
                .roster
                .actions()
                .iter()
                .position(|a| a.id() == "test::move")
                .expect("registered");
            let ctx = DimensionContext::of(&[("position_ref", "P7"), ("lhu", "L1")]);
            s.run_action_chosen(at, &ctx, "L2", window, cx);
        })
    });
    draw(&mut m.vcx);
    assert_eq!(m.depth(), 1, "only the cover");
    assert!(
        m.vcx
            .debug_bounds("action-question-Move to LHU L2?")
            .is_none()
    );
    assert_eq!(
        m.notice().as_deref(),
        Some(crate::shell::row_menu::CONFIRM_REFUSED)
    );
}

/// An empty reply to a choice another dialog covers drops the choice from
/// the stack at once: closing the cover leaves no dead "loading…" dialog.
#[gpui::test]
fn an_empty_reply_to_a_covered_choice_removes_it(cx: &mut gpui::TestAppContext) {
    let mut m = moving(cx);
    let tag = m.tag();
    m.cover();
    assert_eq!(m.depth(), 2, "the cover sits on the loading choice");
    m.deliver(tag, Ok(vec![("L1", 3)]));
    assert_eq!(m.notice().as_deref(), Some("no LHU values to move to"));
    assert_eq!(m.depth(), 1, "only the cover is left");
    assert!(m.target().is_none(), "the choice state is gone");
    m.close_top();
    assert_eq!(m.depth(), 0, "no dead loading dialog under the cover");
    assert!(m.vcx.debug_bounds("action-loading").is_none());
    assert_eq!(m.notice().as_deref(), Some("no LHU values to move to"));
}

/// A query typed while loading still filters the values when they arrive
/// under a cover: it is read from the choice's own stack entry, not from
/// the cover's field.
#[gpui::test]
fn a_covered_choice_filters_by_its_own_query(cx: &mut gpui::TestAppContext) {
    let mut m = moving(cx);
    m.vcx.simulate_input("3");
    draw(&mut m.vcx);
    m.cover();
    m.deliver_three();
    m.close_top();
    assert_eq!(m.depth(), 1, "the choice is back on top");
    assert!(m.vcx.debug_bounds("action-choice-L2").is_none(), "filtered");
    assert!(m.vcx.debug_bounds("action-choice-L3").is_some());
}

#[gpui::test]
fn picking_a_value_asks_to_confirm_and_yes_runs(cx: &mut gpui::TestAppContext) {
    let mut m = moving(cx);
    m.deliver_three();
    m.vcx.simulate_keystrokes("enter");
    draw(&mut m.vcx);
    // The choice list closed before the confirm opened: the confirm is the
    // stack's only entry, never pushed over the list.
    assert_eq!(m.depth(), 1, "the confirm replaced the choice list");
    assert!(m.target().is_none(), "the choice state is gone");
    assert!(
        m.vcx
            .debug_bounds("action-question-Move to LHU L2?")
            .is_some(),
        "asks"
    );
    assert!(m.confirmed.borrow().is_empty(), "nothing runs before yes");
    m.vcx.simulate_keystrokes("y");
    draw(&mut m.vcx);
    assert_eq!(m.confirmed.borrow().as_slice(), &["L2".to_string()]);
    assert_eq!(m.notice().as_deref(), Some("confirmed L2"));
    assert_eq!(m.depth(), 0, "closed");
}

#[gpui::test]
fn no_or_escape_closes_the_confirm_and_runs_nothing(cx: &mut gpui::TestAppContext) {
    for key in ["n", "escape"] {
        let mut m = moving(cx);
        m.deliver_three();
        m.vcx.simulate_keystrokes("enter");
        draw(&mut m.vcx);
        assert_eq!(m.depth(), 1, "{key}: confirm open");
        m.vcx.simulate_keystrokes(key);
        draw(&mut m.vcx);
        assert!(m.confirmed.borrow().is_empty(), "{key}: nothing ran");
        assert_eq!(m.depth(), 0, "{key}: no dialog left open");
    }
}

/// The same choose-then-confirm driven by the mouse: a click on the row
/// menu's action opens the choice, typing reaches its field, a click on a
/// value opens the confirm, and a typed `y` answers it. The field keeps
/// focus through the opening mouse-down because the menu row stops
/// propagation inside an occluding menu; the dialog open's
/// `prevent_default` is a second guard, not the one this path relies on.
#[gpui::test]
fn a_clicked_action_takes_typing_in_its_choice_and_its_confirm(cx: &mut gpui::TestAppContext) {
    let mut m = moving_unopened(cx);
    m.vcx.simulate_keystrokes("g .");
    draw(&mut m.vcx);
    let titles = row_menu_titles(&m.shell, &m.vcx).expect("the row menu is open");
    let at = titles
        .iter()
        .position(|t| t == "Move to LHU\u{2026}")
        .expect("the action's row");
    let row = m
        .vcx
        .debug_bounds(Box::leak(format!("row-menu-row-{at}").into_boxed_str()))
        .expect("the action's row paints");
    m.vcx.simulate_click(row.center(), gpui::Modifiers::none());
    draw(&mut m.vcx);
    assert!(
        row_menu_titles(&m.shell, &m.vcx).is_none(),
        "the menu closed"
    );
    assert_eq!(m.requested.borrow().len(), 1, "the click asked for values");

    m.deliver_three();
    assert_eq!(m.rows(), Some(vec!["L2".to_string(), "L3".to_string()]));
    m.vcx.simulate_input("3");
    draw(&mut m.vcx);
    assert!(m.vcx.debug_bounds("action-choice-L2").is_none(), "narrowed");
    assert!(m.vcx.debug_bounds("action-choice-L3").is_some());
    let shell = m.shell.clone();
    let focused = m.vcx.update(|window, cx| {
        let input = shell.read(cx).dialog_input.clone();
        input.read(cx).focus_handle(cx).is_focused(window)
    });
    assert!(focused, "the choice's field holds focus after the click");

    let choice = m
        .vcx
        .debug_bounds("action-choice-L3")
        .expect("the L3 row paints");
    m.vcx
        .simulate_click(choice.center(), gpui::Modifiers::none());
    draw(&mut m.vcx);
    assert!(
        m.vcx
            .debug_bounds("action-question-Move to LHU L3?")
            .is_some(),
        "the click asks to confirm"
    );
    assert!(m.confirmed.borrow().is_empty(), "nothing runs before yes");
    m.vcx.simulate_keystrokes("y");
    draw(&mut m.vcx);
    assert_eq!(m.confirmed.borrow().as_slice(), &["L3".to_string()]);
    assert_eq!(m.notice().as_deref(), Some("confirmed L3"));
    assert_eq!(m.depth(), 0, "closed");
}

/// A position-service answer, as the app's drain hands it to the shell.
fn lhu_outcome(count: usize, result: Result<(), String>) -> geode_core::positions::CommandOutcome {
    geode_core::positions::CommandOutcome {
        tag: 1,
        count,
        lhu: "BK003_LHU2".into(),
        result,
    }
}

fn noted(
    cx: &mut gpui::TestAppContext,
    outcome: &geode_core::positions::CommandOutcome,
) -> Option<String> {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    shell.update(&mut vcx, |s, cx| s.note_command(outcome, cx));
    shell.read_with(&vcx, |s, _| s.notice.as_ref().map(|n| n.to_string()))
}

#[gpui::test]
fn an_accepted_command_reads_accepted(cx: &mut gpui::TestAppContext) {
    let o = lhu_outcome(3, Ok(()));
    let expected = geode_core::positions::outcome_notice(&o);
    assert_eq!(
        expected,
        "moving 3 positions to LHU BK003_LHU2 \u{b7} accepted"
    );
    assert_eq!(noted(cx, &o), Some(expected));
}

#[gpui::test]
fn a_refused_command_reads_refused(cx: &mut gpui::TestAppContext) {
    let o = lhu_outcome(1, Err("unknown position P99".into()));
    let expected = geode_core::positions::outcome_notice(&o);
    assert_eq!(
        expected,
        "move to LHU BK003_LHU2 refused: unknown position P99"
    );
    assert_eq!(noted(cx, &o), Some(expected));
}

const TWO_COLORS: &str = "[amber]\nhue = 40\n[blue]\nhue = 240\n";

fn color_services(colors: &str) -> ShellServices {
    let mut rec = RecordingFactory::new("rec");
    rec.fragment = Some(ROW_FRAGMENT);
    *rec.dimension_context.borrow_mut() = Some(spx_own());
    let mut services = services_with_recorders(vec![rec]);
    // Assigned after the keymap is built, as reload.rs's fixtures do: only
    // the readers of `config` see it.
    services.config = geode_core::config::Config::from_docs(vec![
        geode_core::config::LayerDoc::builtin("colors", colors).unwrap(),
    ]);
    services
}

/// `g .` on SPX's own row, then enter on `Color…`.
fn open_color_list(
    cx: &mut gpui::TestAppContext,
    colors: &str,
    user_dir: Option<std::path::PathBuf>,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    open_color_list_on(cx, color_services(colors), user_dir)
}

/// [`open_color_list`] over `services` as given.
fn open_color_list_on(
    cx: &mut gpui::TestAppContext,
    services: ShellServices,
    user_dir: Option<std::path::PathBuf>,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    shell.update(&mut vcx, |s, _| {
        s.text_dims.insert("underlying_ref".into());
        s.user_dir = user_dir;
    });
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    vcx.simulate_keystrokes("g .");
    draw(&mut vcx);
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    (shell, vcx)
}

fn shell_notice(shell: &Entity<ShellView>, vcx: &gpui::VisualTestContext) -> Option<String> {
    shell.read_with(vcx, |s, _| s.notice.clone().map(|n| n.to_string()))
}

/// Press `up` until the lit row reads `text` (at most one lap of the list).
fn light_row(shell: &Entity<ShellView>, vcx: &mut gpui::VisualTestContext, text: &str) {
    for _ in 0..40 {
        let lit = shell.read_with(vcx, |s, _| {
            s.choice_dialog
                .as_ref()
                .and_then(|d| d.list.highlighted_text().map(str::to_string))
        });
        if lit.as_deref() == Some(text) {
            return;
        }
        vcx.simulate_keystrokes("up");
    }
    panic!("no row reads {text:?}");
}

fn hue_stage(
    shell: &Entity<ShellView>,
    vcx: &gpui::VisualTestContext,
) -> Option<crate::shell::choicedialog::HueStage> {
    shell.read_with(vcx, |s, _| {
        match s.choice_dialog.as_ref().map(|d| &d.target) {
            Some(crate::shell::choicedialog::Target::ValueColor { stage, .. }) => {
                stage.as_deref().cloned()
            }
            _ => None,
        }
    })
}

fn open_stage(shell: &Entity<ShellView>, vcx: &mut gpui::VisualTestContext) {
    light_row(shell, vcx, "Custom\u{2026}");
    vcx.simulate_keystrokes("enter");
    draw(vcx);
}

fn value_colors_file(dir: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(dir.join("value_colors.toml")).ok()
}

#[gpui::test]
fn color_opens_the_pick_list_with_swatches(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, None);
    assert!(
        shell.read_with(&vcx, |s, _| s.row_menu.is_none()),
        "the menu closed"
    );
    let options = shell.read_with(&vcx, |s, _| {
        s.choice_dialog.as_ref().map(|d| d.list.options().to_vec())
    });
    let options = options.expect("the list is open");
    assert_eq!(&options[..2], ["amber", "blue"]);
    assert_eq!(options.last().map(String::as_str), Some("None"));
    assert!(
        vcx.debug_bounds("valuecolor-swatch-blue").is_some(),
        "a swatch per color"
    );
    assert!(
        vcx.debug_bounds("valuecolor-swatch-None").is_none(),
        "none for None"
    );
    assert!(vcx.debug_bounds("valuecolor-choice-None").is_some());
    assert!(vcx.debug_bounds("valuecolor-empty").is_none());
}

#[gpui::test]
fn picking_a_color_writes_the_user_layer_and_says_so(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, Some(dir.path().to_path_buf()));
    light_row(&shell, &mut vcx, "blue");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    draw(&mut vcx);
    assert!(
        shell.read_with(&vcx, |s, _| s.choice_dialog.is_none()),
        "closed"
    );
    let text = std::fs::read_to_string(dir.path().join("value_colors.toml")).unwrap();
    assert!(
        text.contains("[underlying_ref]") && text.contains("SPX = \"blue\""),
        "{text}"
    );
    assert_eq!(shell_notice(&shell, &vcx), Some("SPX colored blue".into()));
}

#[gpui::test]
fn a_clicked_color_row_writes_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, Some(dir.path().to_path_buf()));
    let at = vcx
        .debug_bounds("valuecolor-choice-amber")
        .expect("the amber row is painted")
        .center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    vcx.run_until_parked();
    let text = std::fs::read_to_string(dir.path().join("value_colors.toml")).unwrap();
    assert!(text.contains("SPX = \"amber\""), "{text}");
    assert_eq!(shell_notice(&shell, &vcx), Some("SPX colored amber".into()));
}

#[gpui::test]
fn enter_on_the_untouched_list_writes_nothing(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, Some(dir.path().to_path_buf()));
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.choice_dialog.is_none()),
        "closed"
    );
    assert!(!dir.path().join("value_colors.toml").exists());
    assert_eq!(shell_notice(&shell, &vcx), None);
}

/// The desk maps SPX to a color `colors.toml` no longer defines: nothing
/// paints, the list opens on `None`, and enter on it is no change. Writing
/// `none` would leave a user override masking the desk once it defines the
/// color again.
#[gpui::test]
fn enter_over_an_undefined_desk_color_writes_nothing(cx: &mut gpui::TestAppContext) {
    use geode_core::config::{Config, Layer, LayerDoc, VALUE_COLORS_DOC};
    let dir = tempfile::tempdir().unwrap();
    let mut services = color_services(TWO_COLORS);
    services.config = Config::from_docs(vec![
        LayerDoc::builtin("colors", TWO_COLORS).unwrap(),
        LayerDoc {
            layer: Layer::Desk,
            name: VALUE_COLORS_DOC.into(),
            file: "value_colors.toml".into(),
            table: "[underlying_ref]\nSPX = \"gone\"\n".parse().unwrap(),
        },
    ]);
    let (shell, mut vcx) = open_color_list_on(cx, services, Some(dir.path().to_path_buf()));
    let lit = shell.read_with(&vcx, |s, _| {
        s.choice_dialog
            .as_ref()
            .and_then(|d| d.list.highlighted_text().map(str::to_string))
    });
    assert_eq!(lit.as_deref(), Some("None"), "opens on None");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.choice_dialog.is_none()),
        "closed"
    );
    assert!(!dir.path().join("value_colors.toml").exists());
    assert_eq!(shell_notice(&shell, &vcx), None);
}

#[gpui::test]
fn a_pick_with_no_user_directory_says_so(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, None);
    light_row(&shell, &mut vcx, "blue");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(
        shell_notice(&shell, &vcx),
        Some(crate::shell::value_color::NO_USER_DIR.to_string())
    );
}

#[gpui::test]
fn a_failed_write_shows_the_writers_error(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("value_colors.toml");
    let before = "config_version = 1\nunderlying_ref = \"blue\"\n";
    std::fs::write(&path, before).unwrap();
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, Some(dir.path().to_path_buf()));
    light_row(&shell, &mut vcx, "blue");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    let notice = shell_notice(&shell, &vcx);
    assert!(
        notice
            .as_deref()
            .is_some_and(|n| n.contains("is not a table")),
        "{notice:?}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "unchanged");
}

/// With no named color the list begins with the presets and offers
/// `New named color…` where the muted "define one" line stood.
#[gpui::test]
fn with_no_named_colors_the_list_offers_a_new_named_color(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_color_list(cx, "", None);
    let options = shell
        .read_with(&vcx, |s, _| {
            s.choice_dialog.as_ref().map(|d| d.list.options().to_vec())
        })
        .expect("the list is open");
    assert_eq!(options[0], "preset \u{b7} red");
    assert_eq!(options.last().map(String::as_str), Some("None"));
    assert!(
        vcx.debug_bounds("valuecolor-choice-New named color\u{2026}")
            .is_some()
    );
    assert!(vcx.debug_bounds("valuecolor-empty").is_none());
}

/// Typing a hue then clearing it lights the preset in force again: enter
/// on the cleared list writes nothing.
#[gpui::test]
fn a_cleared_hue_query_then_enter_writes_nothing(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = services_with_spx(TWO_COLORS, "{ hue = 240 }");
    let (shell, mut vcx) = open_color_list_on(cx, services, Some(dir.path().to_path_buf()));
    vcx.simulate_input("2");
    draw(&mut vcx);
    vcx.simulate_keystrokes("backspace");
    draw(&mut vcx);
    let lit = shell.read_with(&vcx, |s, _| {
        s.choice_dialog
            .as_ref()
            .and_then(|d| d.list.highlighted_text().map(str::to_string))
    });
    assert_eq!(lit.as_deref(), Some("preset \u{b7} blue"));
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.choice_dialog.is_none()),
        "closed"
    );
    assert!(value_colors_file(dir.path()).is_none());
    assert_eq!(shell_notice(&shell, &vcx), None);
}

fn modal_kinds(
    shell: &Entity<ShellView>,
    vcx: &gpui::VisualTestContext,
) -> Vec<crate::shell::dialog::DialogKind> {
    shell.read_with(vcx, |s, _| s.modals.iter().map(|m| m.kind).collect())
}

fn colors_file(dir: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(dir.join("colors.toml")).ok()
}

/// `New named color…` on SPX's list, then wait out the create's write.
fn create_spx(shell: &Entity<ShellView>, vcx: &mut gpui::VisualTestContext) {
    light_row(shell, vcx, "New named color\u{2026}");
    vcx.simulate_keystrokes("enter");
    draw(vcx);
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    super::objectdialog::flush_config_write(vcx);
    vcx.run_until_parked();
}

#[gpui::test]
fn new_named_color_opens_colors_at_naming_with_the_value_prefilled(cx: &mut gpui::TestAppContext) {
    use crate::shell::dialog::DialogKind;
    use crate::shell::objectdialog::{Domain, NameSeed, Stage};
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, None);
    light_row(&shell, &mut vcx, "New named color\u{2026}");
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert_eq!(
        modal_kinds(&shell, &vcx),
        [DialogKind::Choice, DialogKind::Object]
    );
    let (domain, stage, query, seed) = shell.read_with(&vcx, |s, _| {
        let d = s.object_dialog.as_ref().unwrap();
        (
            d.domain,
            d.stage.clone(),
            d.query.clone(),
            d.naming_seed.clone(),
        )
    });
    assert_eq!((domain, stage), (Domain::Colors, Stage::Naming));
    assert_eq!(query, "spx");
    assert_eq!(
        seed,
        NameSeed::Definition(geode_core::colour::Definition::hue(240.0, Tone::Normal))
    );
    let field = shell.read_with(&vcx, |s, cx| s.dialog_input.read(cx).value().to_string());
    assert_eq!(field, "spx", "the name field holds the prefill");
}

#[gpui::test]
fn new_named_color_creates_the_color_and_colors_the_value(cx: &mut gpui::TestAppContext) {
    use crate::shell::dialog::DialogKind;
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, Some(dir.path().to_path_buf()));
    create_spx(&shell, &mut vcx);
    let colors = colors_file(dir.path()).expect("the color was written");
    assert!(
        colors.contains("[spx]") && colors.contains("hue = 240"),
        "{colors}"
    );
    let values = value_colors_file(dir.path()).expect("the value was written");
    assert!(values.contains("SPX = \"spx\""), "{values}");
    assert_eq!(
        modal_kinds(&shell, &vcx),
        [DialogKind::Object],
        "the covered pick list is gone; Colors continues"
    );
    assert!(shell.read_with(&vcx, |s, _| s.choice_dialog.is_none()));
    assert_eq!(shell_notice(&shell, &vcx), Some("SPX colored spx".into()));
    assert!(
        shell.read_with(&vcx, |s, _| s
            .object_dialog
            .as_ref()
            .is_some_and(|d| d.on_created.is_none())),
        "the create took the one-shot hook"
    );
}

/// A typed hue keeps `New named color…` reachable beneath its row, and
/// the color it creates starts from the typed hue.
#[gpui::test]
fn a_typed_hue_seeds_a_new_named_color(cx: &mut gpui::TestAppContext) {
    use crate::shell::dialog::DialogKind;
    use crate::shell::objectdialog::{Domain, NameSeed, Stage};
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, Some(dir.path().to_path_buf()));
    vcx.simulate_input("210");
    draw(&mut vcx);
    vcx.simulate_keystrokes("down");
    draw(&mut vcx);
    let lit = shell.read_with(&vcx, |s, _| {
        s.choice_dialog
            .as_ref()
            .and_then(|d| d.list.highlighted_text().map(str::to_string))
    });
    assert_eq!(lit.as_deref(), Some("New named color\u{2026}"));
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert_eq!(
        modal_kinds(&shell, &vcx),
        [DialogKind::Choice, DialogKind::Object]
    );
    let (domain, stage, seed) = shell.read_with(&vcx, |s, _| {
        let d = s.object_dialog.as_ref().unwrap();
        (d.domain, d.stage.clone(), d.naming_seed.clone())
    });
    assert_eq!((domain, stage), (Domain::Colors, Stage::Naming));
    assert_eq!(
        seed,
        NameSeed::Definition(geode_core::colour::Definition::hue(210.0, Tone::Normal))
    );
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    super::objectdialog::flush_config_write(&mut vcx);
    vcx.run_until_parked();
    let colors = colors_file(dir.path()).expect("the color was written");
    assert!(
        colors.contains("[spx]") && colors.contains("hue = 210"),
        "{colors}"
    );
    let values = value_colors_file(dir.path()).expect("the value was written");
    assert!(values.contains("SPX = \"spx\""), "{values}");
}

#[gpui::test]
fn the_hook_colors_the_value_once_and_a_later_create_leaves_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, Some(dir.path().to_path_buf()));
    create_spx(&shell, &mut vcx);
    // Escape out of the edit stage to browse, then the dialog's own `n`.
    for _ in 0..3 {
        let browsing = shell.read_with(&vcx, |s, _| {
            s.object_dialog
                .as_ref()
                .is_some_and(|d| d.stage == crate::shell::objectdialog::Stage::Browse)
        });
        if browsing {
            break;
        }
        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();
    }
    vcx.simulate_keystrokes("n");
    vcx.simulate_input("other");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    super::objectdialog::flush_config_write(&mut vcx);
    vcx.run_until_parked();
    assert!(colors_file(dir.path()).unwrap().contains("[other]"));
    let values = value_colors_file(dir.path()).unwrap();
    assert!(
        values.contains("SPX = \"spx\"") && !values.contains("other"),
        "{values}"
    );
}

#[gpui::test]
fn escape_at_naming_pops_only_when_opened_from_the_pick_list(cx: &mut gpui::TestAppContext) {
    use crate::shell::dialog::DialogKind;
    use crate::shell::objectdialog::Stage;
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, Some(dir.path().to_path_buf()));
    let before = shell.read_with(&vcx, |s, _| {
        s.choice_dialog.as_ref().map(|d| d.list.options().to_vec())
    });
    light_row(&shell, &mut vcx, "New named color\u{2026}");
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert_eq!(
        modal_kinds(&shell, &vcx),
        [DialogKind::Choice],
        "back on the list"
    );
    let after = shell.read_with(&vcx, |s, _| {
        s.choice_dialog.as_ref().map(|d| d.list.options().to_vec())
    });
    assert_eq!(after, before, "intact");
    let lit = shell.read_with(&vcx, |s, _| {
        s.choice_dialog
            .as_ref()
            .and_then(|d| d.list.highlighted_text().map(str::to_string))
    });
    assert_eq!(lit.as_deref(), Some("New named color\u{2026}"));
    assert!(colors_file(dir.path()).is_none() && value_colors_file(dir.path()).is_none());
    // The Colors dialog's own `n` still escapes to browse.
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    shell.update_in(&mut vcx, |s, window, cx| {
        s.dispatch(
            &crate::actions::ActionId("config::colors".into()),
            None,
            window,
            cx,
        )
    });
    vcx.run_until_parked();
    vcx.simulate_keystrokes("n escape");
    vcx.run_until_parked();
    assert_eq!(modal_kinds(&shell, &vcx), [DialogKind::Object]);
    let stage = shell.read_with(&vcx, |s, _| {
        s.object_dialog.as_ref().map(|d| d.stage.clone())
    });
    assert_eq!(stage, Some(Stage::Browse));
}

/// The title row's `‹` at naming opened from the list pops back to it, as
/// escape does.
#[gpui::test]
fn back_at_naming_returns_to_the_pick_list(cx: &mut gpui::TestAppContext) {
    use crate::shell::dialog::DialogKind;
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, None);
    light_row(&shell, &mut vcx, "New named color\u{2026}");
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    shell.update_in(&mut vcx, |s, window, cx| {
        crate::shell::dialog::step_back(s, window, cx)
    });
    vcx.run_until_parked();
    assert_eq!(modal_kinds(&shell, &vcx), [DialogKind::Choice]);
}

#[gpui::test]
fn a_taken_prefilled_name_is_refused_and_colors_nothing(cx: &mut gpui::TestAppContext) {
    use crate::shell::dialog::DialogKind;
    let dir = tempfile::tempdir().unwrap();
    let colors = "[amber]\nhue = 40\n[blue]\nhue = 240\n[spx]\nhue = 1\n";
    let (shell, mut vcx) = open_color_list(cx, colors, Some(dir.path().to_path_buf()));
    light_row(&shell, &mut vcx, "New named color\u{2026}");
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    let notice = shell.read_with(&vcx, |s, _| {
        s.object_dialog.as_ref().and_then(|d| d.notice.clone())
    });
    assert!(
        notice
            .as_deref()
            .is_some_and(|n| n.contains("'spx' already exists")),
        "{notice:?}"
    );
    assert_eq!(
        modal_kinds(&shell, &vcx),
        [DialogKind::Choice, DialogKind::Object]
    );
    assert!(value_colors_file(dir.path()).is_none());
}

#[gpui::test]
fn a_failed_color_create_leaves_the_value_uncolored(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("colors.toml"), "this is [[[ not toml\n").unwrap();
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, Some(dir.path().to_path_buf()));
    create_spx(&shell, &mut vcx);
    assert!(
        value_colors_file(dir.path()).is_none(),
        "no value write after a failed create"
    );
    assert!(shell.read_with(&vcx, |s, _| s.config_write_error.is_some()));
}

#[gpui::test]
fn custom_opens_the_hue_stage_and_enter_applies_the_stepped_hue(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, Some(dir.path().to_path_buf()));
    open_stage(&shell, &mut vcx);
    let stage = hue_stage(&shell, &vcx).expect("the stage is open");
    assert_eq!(
        (stage.hue, stage.tone),
        (240, Tone::Normal),
        "no color in force: hue 240"
    );
    assert!(vcx.debug_bounds("valuecolor-stage-preview").is_some());
    assert!(vcx.debug_bounds("valuecolor-stage-slider").is_some());
    assert!(
        vcx.debug_bounds("shell-modal-back").is_some(),
        "the back button paints"
    );
    vcx.simulate_keystrokes("l shift-h t");
    draw(&mut vcx);
    let stage = hue_stage(&shell, &vcx).unwrap();
    assert_eq!((stage.hue, stage.tone), (254, Tone::Light));
    assert!(
        value_colors_file(dir.path()).is_none(),
        "no write before Apply"
    );
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.choice_dialog.is_none()),
        "closed"
    );
    let text = value_colors_file(dir.path()).unwrap();
    assert!(
        text.contains("SPX = { hue = 254, tone = \"light\" }"),
        "{text}"
    );
    assert_eq!(
        shell_notice(&shell, &vcx),
        Some("SPX colored hue 254 light".into())
    );
}

#[gpui::test]
fn escape_from_the_hue_stage_writes_nothing_and_returns_to_the_list(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, Some(dir.path().to_path_buf()));
    open_stage(&shell, &mut vcx);
    vcx.simulate_keystrokes("l escape");
    draw(&mut vcx);
    assert!(hue_stage(&shell, &vcx).is_none(), "the stage closed");
    let lit = shell.read_with(&vcx, |s, _| {
        s.choice_dialog
            .as_ref()
            .and_then(|d| d.list.highlighted_text().map(str::to_string))
    });
    assert_eq!(lit.as_deref(), Some("Custom\u{2026}"), "the list as it was");
    assert!(shell.read_with(&vcx, |s, _| s.hue_slider.is_none()));
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.choice_dialog.is_none()),
        "a second escape closes"
    );
    assert!(value_colors_file(dir.path()).is_none());
    assert_eq!(shell_notice(&shell, &vcx), None);
}

#[gpui::test]
fn the_back_button_leaves_the_hue_stage(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, None);
    open_stage(&shell, &mut vcx);
    let at = vcx.debug_bounds("shell-modal-back").unwrap().center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    draw(&mut vcx);
    assert!(hue_stage(&shell, &vcx).is_none());
    assert!(
        shell.read_with(&vcx, |s, _| s.choice_dialog.is_some()),
        "still the list"
    );
}

#[gpui::test]
fn an_out_of_range_hue_keeps_apply_from_writing(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, Some(dir.path().to_path_buf()));
    open_stage(&shell, &mut vcx);
    vcx.simulate_keystrokes("4 0 0");
    draw(&mut vcx);
    assert!(vcx.debug_bounds("valuecolor-stage-refusal").is_some());
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(hue_stage(&shell, &vcx).is_some(), "still on the stage");
    assert!(value_colors_file(dir.path()).is_none());
    vcx.simulate_keystrokes("backspace enter");
    vcx.run_until_parked();
    let text = value_colors_file(dir.path()).unwrap();
    assert!(text.contains("SPX = { hue = 40 }"), "{text}");
}

#[gpui::test]
fn a_slider_click_and_a_tone_click_set_the_stage(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, None);
    open_stage(&shell, &mut vcx);
    let at = vcx
        .debug_bounds("valuecolor-stage-slider")
        .unwrap()
        .center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    draw(&mut vcx);
    let hue = hue_stage(&shell, &vcx).unwrap().hue;
    assert!((170..=190).contains(&hue), "the middle of the track: {hue}");
    let tone = vcx.debug_bounds("valuecolor-stage-tone").unwrap();
    vcx.simulate_click(
        gpui::point(tone.right() - gpui::px(4.), tone.center().y),
        gpui::Modifiers::none(),
    );
    draw(&mut vcx);
    assert_eq!(hue_stage(&shell, &vcx).unwrap().tone, Tone::Light);
}

/// The stage opening and a tone change resolve the slider track in their
/// handlers, so a paint only reads the cache: by key and by click.
#[gpui::test]
fn a_tone_change_warms_the_track_before_the_paint(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, None);
    open_stage(&shell, &mut vcx);
    let signature = vcx.update(|_, cx| {
        use gpui_component::ActiveTheme as _;
        crate::shell::colours::theme_signature(cx.theme())
    });
    let misses =
        |vcx: &gpui::VisualTestContext| hue_stage(&shell, vcx).map(|s| s.cache.paint_misses());
    assert_eq!(misses(&vcx), Some(0), "the stage opened warm");
    vcx.simulate_keystrokes("t");
    draw(&mut vcx);
    let stage = hue_stage(&shell, &vcx).unwrap();
    assert_eq!(stage.tone, Tone::Light);
    assert!(stage.cache.holds_track(signature, Tone::Light));
    assert_eq!(misses(&vcx), Some(0), "the tone key warmed the track");
    let tone = vcx.debug_bounds("valuecolor-stage-tone").unwrap();
    vcx.simulate_click(
        gpui::point(tone.left() + gpui::px(4.), tone.center().y),
        gpui::Modifiers::none(),
    );
    draw(&mut vcx);
    assert_eq!(hue_stage(&shell, &vcx).unwrap().tone, Tone::Normal);
    assert_eq!(misses(&vcx), Some(0), "the tone click warmed the track");
}

#[gpui::test]
fn a_clicked_apply_writes_and_apply_on_the_color_in_force_writes_nothing(
    cx: &mut gpui::TestAppContext,
) {
    use geode_core::config::{Config, Layer, LayerDoc, VALUE_COLORS_DOC};
    let dir = tempfile::tempdir().unwrap();
    let mut services = color_services(TWO_COLORS);
    services.config = Config::from_docs(vec![
        LayerDoc::builtin("colors", TWO_COLORS).unwrap(),
        LayerDoc {
            layer: Layer::User,
            name: VALUE_COLORS_DOC.into(),
            file: "value_colors.toml".into(),
            table: "[underlying_ref]\nSPX = { hue = 200 }\n".parse().unwrap(),
        },
    ]);
    let (shell, mut vcx) = open_color_list_on(cx, services, Some(dir.path().to_path_buf()));
    let lit = shell.read_with(&vcx, |s, _| {
        s.choice_dialog
            .as_ref()
            .and_then(|d| d.list.highlighted_text().map(str::to_string))
    });
    assert_eq!(
        lit.as_deref(),
        Some("Custom\u{2026}"),
        "an inline entry opens on Custom…"
    );
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert_eq!(
        hue_stage(&shell, &vcx).unwrap().hue,
        200,
        "the stage opens on it"
    );
    let apply = vcx.debug_bounds("valuecolor-apply").unwrap().center();
    vcx.simulate_click(apply, gpui::Modifiers::none());
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.choice_dialog.is_none()),
        "Apply closes"
    );
    assert!(
        value_colors_file(dir.path()).is_none(),
        "the color in force: nothing written"
    );
    assert_eq!(shell_notice(&shell, &vcx), None);
}

/// The palette opened over the hue stage and closed again hands the keys
/// back to the stage: its list's field is not painted, so focusing it
/// would leave no surface listening.
#[gpui::test]
fn the_stage_keeps_its_keys_after_the_palette_closes(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, None);
    open_stage(&shell, &mut vcx);
    vcx.simulate_keystrokes("ctrl-k");
    draw(&mut vcx);
    assert!(
        shell.read_with(&vcx, |s, _| s.palette.is_some()),
        "the palette opened"
    );
    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);
    assert!(
        shell.read_with(&vcx, |s, _| s.palette.is_none()),
        "the palette closed"
    );
    vcx.simulate_keystrokes("l");
    draw(&mut vcx);
    assert_eq!(hue_stage(&shell, &vcx).map(|s| s.hue), Some(255));
}

#[gpui::test]
fn a_preset_pick_writes_its_hue_and_says_preset(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, Some(dir.path().to_path_buf()));
    assert!(
        vcx.debug_bounds("valuecolor-swatch-preset-red").is_some(),
        "a preset swatch"
    );
    light_row(&shell, &mut vcx, "preset \u{b7} red");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    let text = value_colors_file(dir.path()).unwrap();
    assert!(text.contains("SPX = { hue = 0 }"), "{text}");
    assert_eq!(
        shell_notice(&shell, &vcx),
        Some("SPX colored red preset".into())
    );
}

#[gpui::test]
fn typing_a_hue_then_enter_writes_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut vcx) = open_color_list(cx, TWO_COLORS, Some(dir.path().to_path_buf()));
    vcx.simulate_input("210");
    draw(&mut vcx);
    assert!(vcx.debug_bounds("valuecolor-choice-Hue 210").is_some());
    assert!(vcx.debug_bounds("valuecolor-swatch-hue").is_some());
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    let text = value_colors_file(dir.path()).unwrap();
    assert!(text.contains("SPX = { hue = 210 }"), "{text}");
    assert_eq!(
        shell_notice(&shell, &vcx),
        Some("SPX colored hue 210".into())
    );
}

/// [`color_services`] over `colors` with `entry` as SPX's user-layer entry.
fn services_with_spx(colors: &str, entry: &str) -> ShellServices {
    use geode_core::config::{Config, Layer, LayerDoc, VALUE_COLORS_DOC};
    let mut services = color_services(colors);
    services.config = Config::from_docs(vec![
        LayerDoc::builtin("colors", colors).unwrap(),
        LayerDoc {
            layer: Layer::User,
            name: VALUE_COLORS_DOC.into(),
            file: "value_colors.toml".into(),
            table: format!("[underlying_ref]\nSPX = {entry}\n")
                .parse()
                .unwrap(),
        },
    ]);
    services
}

#[gpui::test]
fn enter_on_an_untouched_preset_writes_nothing(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = services_with_spx(TWO_COLORS, "{ hue = 240 }");
    let (shell, mut vcx) = open_color_list_on(cx, services, Some(dir.path().to_path_buf()));
    let lit = shell.read_with(&vcx, |s, _| {
        s.choice_dialog
            .as_ref()
            .and_then(|d| d.list.highlighted_text().map(str::to_string))
    });
    assert_eq!(lit.as_deref(), Some("preset \u{b7} blue"));
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.choice_dialog.is_none()),
        "closed"
    );
    assert!(value_colors_file(dir.path()).is_none());
    assert_eq!(shell_notice(&shell, &vcx), None);
}

/// A named color in force, the stage opened on it and applied untouched:
/// nothing is written, so the value stays on the name rather than being
/// detached onto an inline copy of its hue.
#[gpui::test]
fn an_untouched_apply_over_a_named_color_writes_nothing(cx: &mut gpui::TestAppContext) {
    const SPX_COLORS: &str = "[amber]\nhue = 40\n[spx]\nhue = 210\n";
    let dir = tempfile::tempdir().unwrap();
    let services = services_with_spx(SPX_COLORS, "\"spx\"");
    let (shell, mut vcx) = open_color_list_on(cx, services, Some(dir.path().to_path_buf()));
    open_stage(&shell, &mut vcx);
    assert_eq!(
        hue_stage(&shell, &vcx).map(|s| (s.hue, s.tone)),
        Some((210, Tone::Normal)),
        "the stage opens on the name's hue"
    );
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.choice_dialog.is_none()),
        "Apply closes"
    );
    assert!(value_colors_file(dir.path()).is_none(), "nothing written");
    assert_eq!(shell_notice(&shell, &vcx), None);
}
