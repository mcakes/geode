//! Launching: `TileContent::launched` reaches a tile `add_tile`
//! created (add, duplicate) once it is focused, never a restored one;
//! `tile::open_with` lists the kinds accepting the focused tile's
//! context and creates the pick with the factory's translated state.

use super::*;
use crate::defaults::AddPlacement;
use crate::module::recording::{Recorded, RecordingFactory};
use geode_core::context::DimensionContext;

type Log = std::rc::Rc<std::cell::RefCell<Vec<Recorded>>>;

fn launched(log: &Log, tile: TileId) -> usize {
    log.borrow()
        .iter()
        .filter(|r| matches!(r, Recorded::Launched(t) if *t == tile))
        .count()
}

pub(super) fn draw(cx: &mut gpui::VisualTestContext) {
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.run_until_parked();
}

pub(super) fn focused(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> TileId {
    shell.read_with(cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    })
}

/// An add through the real key route: the new tile is focused on its first
/// render, and hears `launched` exactly once, even across further renders.
#[gpui::test]
fn an_added_tile_hears_launched_once(cx: &mut gpui::TestAppContext) {
    let (services, log) = test_services_with_log();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let tile = focused(&shell, &vcx);
    assert_eq!(launched(&log, tile), 1, "{:?}", log.borrow());
    draw(&mut vcx);
    assert_eq!(launched(&log, tile), 1, "a later render does not repeat it");
}

/// Duplicate goes through `add_tile`, so the copy hears it too.
#[gpui::test]
fn a_duplicated_tile_hears_launched(cx: &mut gpui::TestAppContext) {
    let (services, log) = test_services_with_log();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let first = focused(&shell, &vcx);
    dispatch_action(&shell, "workspace::duplicate_horizontal", &mut vcx);
    draw(&mut vcx);
    let copy = focused(&shell, &vcx);
    assert_ne!(copy, first);
    assert_eq!(launched(&log, copy), 1, "{:?}", log.borrow());
}

/// An add whose tile is no longer focused on its first render (focus moved
/// back before the frame) is not prompted: `launched` may take the keyboard,
/// and only the focused tile may do that.
#[gpui::test]
fn an_add_that_is_not_focused_on_its_first_render_is_not_launched(cx: &mut gpui::TestAppContext) {
    let (services, log) = test_services_with_log();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let first = focused(&shell, &vcx);
    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.add_tile("rec", AddPlacement::Split(None), None, window, cx);
            s.services.workspaces.active_mut().focus_main_tile(first);
        })
    });
    draw(&mut vcx);
    let tiles = shell.read_with(&vcx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles.len(), 2);
    let second = *tiles.iter().find(|t| **t != first).unwrap();
    assert_eq!(launched(&log, second), 0, "{:?}", log.borrow());
}

/// A restored session never hears `launched`: startup takes focus from
/// nothing, however many tiles were saved.
#[gpui::test]
fn a_restored_tile_is_not_launched(cx: &mut gpui::TestAppContext) {
    let mut table = crate::session::to_toml(
        &Workspaces::new(),
        &crate::session::TileRecords::new(),
        None,
        &crate::session::PinnedRecords::new(),
        &crate::palette_usage::PaletteUsage::new(),
        &crate::session::PageRecords::new(),
    );
    let ws1: toml::Table = r#"
        focused = 1
        [node]
        kind = "leaf"
        id = 1
        [tiles.1]
        module = "rec"
    "#
    .parse()
    .unwrap();
    if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
        ws_table.insert("1".to_string(), toml::Value::Table(ws1));
    }
    let restored = crate::session::from_toml(&table).unwrap();
    let (mut services, log) = test_services_with_log();
    services.workspaces = restored.workspaces;
    services.restored_tiles = restored.tiles;
    let (_window, mut vcx) = open_shell(cx, services);
    draw(&mut vcx);
    assert!(
        log.borrow()
            .iter()
            .any(|r| matches!(r, Recorded::Created(TileId(1), _))),
        "fixture: the tile was restored: {:?}",
        log.borrow()
    );
    assert_eq!(launched(&log, TileId(1)), 0, "{:?}", log.borrow());
}

/// Picking a kind in the tile picker: a tile that takes the keyboard in
/// `launched` (as the market-data panel's picker does) still holds it after
/// the modal's focus return and two more frames.
#[gpui::test]
fn a_launched_tile_keeps_the_keyboard_it_takes(cx: &mut gpui::TestAppContext) {
    let mut rec = RecordingFactory::new("rec");
    rec.edit_on_launch = true;
    let input = rec.input.clone();
    let services = services_with_recorders(vec![rec]);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    // The first tile took the keyboard in its own `launched`; give it back
    // (the fixture ships no cancel binding) so the picker opens from the
    // shell, as a trader's `mod+n` would.
    vcx.update(|window, cx| window.blur(cx));
    draw(&mut vcx);
    dispatch_action(&shell, "tile::add", &mut vcx);
    draw(&mut vcx);
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    draw(&mut vcx);
    let new = focused(&shell, &vcx);
    let tiles = shell.read_with(&vcx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles.len(), 2, "the pick split");
    let held = vcx.update(|window, cx| {
        input
            .borrow()
            .as_ref()
            .is_some_and(|i| i.read(cx).focus_handle(cx).is_focused(window))
    });
    assert!(held, "tile {new:?}'s launched input holds the keyboard");
}

/// Binds `g m` in the recorder's own context beside a competing `g g`, as
/// the blotter and pricer fragments do, so the sequence matcher has a real
/// prefix to resolve.
const LAUNCH_FRAGMENT: &str = "[[bindings]]\ncontext = \"rec\"\n[bindings.keys]\n\"g g\" = \"rec::noop\"\n\"g m\" = \"tile::open_with\"\n";

/// A roster of "rec" (the source: reports `context`, accepts the
/// underlying, ships `g m`) and "plain" (accepts nothing). Returns rec's
/// log and its shared context cell.
fn launch_services(
    context: DimensionContext,
) -> (
    ShellServices,
    Log,
    std::rc::Rc<std::cell::RefCell<Option<DimensionContext>>>,
) {
    let mut rec = RecordingFactory::new("rec");
    rec.fragment = Some(LAUNCH_FRAGMENT);
    rec.accepts = &["underlying_ref"];
    *rec.dimension_context.borrow_mut() = Some(context);
    let log = rec.log.clone();
    let cell = rec.dimension_context.clone();
    let plain = RecordingFactory::new("plain");
    let services = services_with_recorders(vec![rec, plain]);
    assert!(
        services.keymap_fragment_diagnostics.is_empty(),
        "{:?}",
        services.keymap_fragment_diagnostics
    );
    (services, log, cell)
}

fn spx() -> DimensionContext {
    DimensionContext::of(&[("underlying_ref", "SPX")])
}

pub(super) fn underlying_state(u: &str) -> toml::Table {
    let mut t = toml::Table::new();
    t.insert(
        "underlying".into(),
        toml::Value::Array(vec![toml::Value::String(u.into())]),
    );
    t
}

fn dialog_target(
    shell: &Entity<ShellView>,
    cx: &gpui::VisualTestContext,
) -> Option<crate::shell::choicedialog::Target> {
    shell.read_with(cx, |s, _| {
        s.choice_dialog.as_ref().map(|d| d.target.clone())
    })
}

/// `g m` on a tile with an underlying opens `Open SPX in…` listing only
/// the accepting kind; `enter` splits a new tile whose `create` received
/// the factory's translated state.
#[gpui::test]
fn g_m_lists_the_accepting_kinds_and_a_pick_creates_with_the_context(
    cx: &mut gpui::TestAppContext,
) {
    let (services, log, _cell) = launch_services(spx());
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    vcx.simulate_keystrokes("g m");
    draw(&mut vcx);
    assert!(
        matches!(
            dialog_target(&shell, &vcx),
            Some(crate::shell::choicedialog::Target::TileKindWith { ref context, .. })
                if *context == spx()
        ),
        "{:?}",
        dialog_target(&shell, &vcx)
    );
    assert!(vcx.debug_bounds("tile-choice-Rec").is_some());
    assert!(
        vcx.debug_bounds("tile-choice-Plain").is_none(),
        "a kind accepting nothing is not listed"
    );
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    let tiles = shell.read_with(&vcx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles.len(), 2, "a pick splits");
    let new = focused(&shell, &vcx);
    let expected = underlying_state("SPX");
    assert!(
        log.borrow()
            .iter()
            .any(|r| matches!(r, Recorded::Created(t, Some(s)) if *t == new && *s == expected)),
        "{:?}",
        log.borrow()
    );
}

/// A blotter row names several columns; a kind accepting one of them is
/// offered, and the dialog is titled by that column's value.
#[gpui::test]
fn g_m_offers_a_kind_accepting_one_of_several_columns(cx: &mut gpui::TestAppContext) {
    let ctx = DimensionContext::of(&[
        ("lhu", "7"),
        ("underlying_ref", "SPX"),
        ("position_ref", "P7"),
    ]);
    let (services, log, _cell) = launch_services(ctx);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    vcx.simulate_keystrokes("g m");
    draw(&mut vcx);
    let title = shell.read_with(&vcx, |s, _| s.choice_dialog.as_ref().map(|d| d.title()));
    assert_eq!(title.as_deref(), Some("Open SPX in\u{2026}"));
    assert!(
        vcx.debug_bounds("tile-choice-Rec").is_some(),
        "lhu and position_ref do not block it"
    );
    assert!(vcx.debug_bounds("tile-choice-Plain").is_none());
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    let new = focused(&shell, &vcx);
    let expected = underlying_state("SPX");
    assert!(
        log.borrow()
            .iter()
            .any(|r| matches!(r, Recorded::Created(t, Some(s)) if *t == new && *s == expected)),
        "{:?}",
        log.borrow()
    );
}

/// The context is read when the dialog opens: a cursor move on the source
/// while it is open does not change what the pick creates.
#[gpui::test]
fn the_context_is_captured_when_the_dialog_opens(cx: &mut gpui::TestAppContext) {
    let (services, log, cell) = launch_services(spx());
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    vcx.simulate_keystrokes("g m");
    draw(&mut vcx);
    *cell.borrow_mut() = Some(DimensionContext::of(&[("underlying_ref", "NDX")]));
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    let new = focused(&shell, &vcx);
    let expected = underlying_state("SPX");
    assert!(
        log.borrow()
            .iter()
            .any(|r| matches!(r, Recorded::Created(t, Some(s)) if *t == new && *s == expected)),
        "{:?}",
        log.borrow()
    );
}

/// An empty context falls back to the plain tile-kind picker.
#[gpui::test]
fn g_m_with_an_empty_context_opens_the_plain_tile_picker(cx: &mut gpui::TestAppContext) {
    let (services, _log, _cell) = launch_services(DimensionContext::default());
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    vcx.simulate_keystrokes("g m");
    draw(&mut vcx);
    assert!(
        matches!(
            dialog_target(&shell, &vcx),
            Some(crate::shell::choicedialog::Target::TileKind { .. })
        ),
        "{:?}",
        dialog_target(&shell, &vcx)
    );
}

/// A row whose context holds only columns no factory registers (an `lhu`
/// subtotal) opens the plain tile picker, as an empty context does, even
/// though a kind accepting another column is on the roster.
#[gpui::test]
fn g_m_on_a_row_with_no_context_column_opens_the_plain_picker(cx: &mut gpui::TestAppContext) {
    let (services, _log, _cell) = launch_services(DimensionContext::of(&[("lhu", "L1")]));
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    vcx.simulate_keystrokes("g m");
    draw(&mut vcx);
    assert!(
        matches!(
            dialog_target(&shell, &vcx),
            Some(crate::shell::choicedialog::Target::TileKind { .. })
        ),
        "{:?}",
        dialog_target(&shell, &vcx)
    );
    assert_eq!(shell.read_with(&vcx, |s, _| s.notice), None);
}

/// A roster where no kind accepts anything registers no context column, so
/// even an `underlying_ref` context opens the plain picker, not the notice.
/// The notice (a registered column no listed kind accepts) is unreachable
/// in Part 1: every registered column comes from some factory's `accepts`,
/// and that factory is then listed. Part 2's action columns make it
/// reachable.
#[gpui::test]
fn g_m_with_no_accepting_kind_opens_the_plain_picker(cx: &mut gpui::TestAppContext) {
    let mut rec = RecordingFactory::new("rec");
    rec.fragment = Some(LAUNCH_FRAGMENT);
    *rec.dimension_context.borrow_mut() = Some(spx());
    let services = services_with_recorders(vec![rec]);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    vcx.simulate_keystrokes("g m");
    draw(&mut vcx);
    assert!(
        matches!(
            dialog_target(&shell, &vcx),
            Some(crate::shell::choicedialog::Target::TileKind { .. })
        ),
        "{:?}",
        dialog_target(&shell, &vcx)
    );
    assert_eq!(shell.read_with(&vcx, |s, _| s.notice), None);
}
