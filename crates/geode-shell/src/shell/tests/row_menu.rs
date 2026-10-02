//! The shell's row menu: `tile::context_menu` (`g .`) on the focused
//! tile's cursor row, its keys, its picks, and what closes it.

use super::drag::main_tile_point;
use super::launch::{draw, focused, underlying_state};
use super::*;
use crate::module::recording::{Recorded, RecordingAction, RecordingFactory};
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
    assert!(m.vcx.debug_bounds("action-hints").is_some(), "the full footer");
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
    assert!(row_menu_titles(&m.shell, &m.vcx).is_none(), "the menu closed");
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
    m.vcx.simulate_click(choice.center(), gpui::Modifiers::none());
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
