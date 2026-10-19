//! The window registry and the opener: `ShellApp` opens a window through
//! the installed opener (reserve, open, confirm or roll back), lists and
//! raises the windows it knows, parks what a closing window hosted, and
//! names each window's title.

use super::occupants::dispatch_and_draw;
use super::*;
use crate::module::recording::Recorded;
use crate::shell::app::{OpenRefusal, WindowOpener};

/// A visual context over a window the app opened through the opener,
/// recovered from the registry by its key.
fn context_for(
    cx: &mut gpui::TestAppContext,
    app: &Entity<ShellApp>,
    key: WindowKey,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    let handle = cx
        .update(|cx| app.read(cx).handle(key))
        .expect("the registry lists the opened window");
    let mut vcx = gpui::VisualTestContext::from_window(handle, cx);
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let root = handle.downcast::<Root>().expect("a Root over a ShellView");
    let shell = shell_of(&root, &mut vcx);
    (shell, vcx)
}

fn shown(app: &Entity<ShellApp>, cx: &gpui::VisualTestContext, key: WindowKey) -> Option<u8> {
    app.read_with(cx, |a, _| {
        a.services().workspaces.shown(key).map(|ix| ix.get())
    })
}

fn parks_in(log: &std::rc::Rc<std::cell::RefCell<Vec<Recorded>>>) -> Vec<TileId> {
    log.borrow()
        .iter()
        .filter_map(|r| match r {
            Recorded::Park(t) => Some(*t),
            _ => None,
        })
        .collect()
}

/// `open_fresh` opens a second window on the lowest index nobody shows:
/// the first shows 1, so the new one shows 2, and the registry lists both.
#[gpui::test]
fn open_fresh_opens_on_the_lowest_free_index(cx: &mut gpui::TestAppContext) {
    let app = test_app(cx, test_services(), None);
    let (w1, mut vcx1) = open_shell_on(cx, app.clone());
    let k1 = shell_of(&w1, &mut vcx1).read_with(&vcx1, |s, _| s.key());
    assert_eq!(shown(&app, &vcx1, k1), Some(1), "sanity");

    // From inside the first window's update, as a dispatched verb runs.
    let opened = vcx1.update(|_, cx| ShellApp::open_fresh(&app, k1, cx));
    let k2 = opened.expect("a free index and an installed opener");
    assert_ne!(k2, k1);
    assert_eq!(shown(&app, &vcx1, k2), Some(2));
    assert_eq!(shown(&app, &vcx1, k1), Some(1));
    assert_eq!(app.read_with(&vcx1, |a, _| a.windows().len()), 2);
    assert!(app.read_with(&vcx1, |a, _| a.handle(k2).is_some()));
    assert!(!app.read_with(&vcx1, |a, _| a.is_last_window(k1)));

    // The opened window is a real window over a ShellView for the key.
    let (s2, vcx2) = context_for(cx, &app, k2);
    assert_eq!(s2.read_with(&vcx2, |s, _| s.key()), k2);
    assert_eq!(
        s2.read_with(&vcx2, |s, cx| s.shown_ix(cx)),
        WorkspaceIx::new(2).unwrap()
    );
}

/// `tear_out` moves the shown workspace into a new window and falls the
/// old window back to the lowest free index: the new window shows 1 and
/// hosts the tile, rebuilt from the record the old window parked, and the
/// old window shows 2.
#[gpui::test]
fn tear_out_moves_the_shown_workspace_and_falls_back(cx: &mut gpui::TestAppContext) {
    let (services, log) = services_with_recorder();
    let app = test_app(cx, services, None);
    let (w1, mut vcx1) = open_shell_on(cx, app.clone());
    let s1 = shell_of(&w1, &mut vcx1);
    let k1 = s1.read_with(&vcx1, |s, _| s.key());
    dispatch_and_draw(&s1, &mut vcx1, "tile::add_rec");
    let tile = s1.read_with(&vcx1, |s, cx| s.workspace(cx).focused_tile().unwrap());
    assert!(s1.read_with(&vcx1, |s, _| s.occupants.contains_key(&tile)));

    let k2 = vcx1
        .update(|_, cx| ShellApp::tear_out(&app, k1, cx))
        .expect("a free index and an installed opener");
    vcx1.run_until_parked();

    assert_eq!(shown(&app, &vcx1, k2), Some(1), "the new window shows 1");
    assert_eq!(
        shown(&app, &vcx1, k1),
        Some(2),
        "the old window fell back to 2"
    );
    assert_eq!(
        app.read_with(&vcx1, |a, _| a
            .services()
            .workspaces
            .host_of(WorkspaceIx::new(1).unwrap())),
        Some(k2),
        "the new window hosts 1"
    );
    assert_eq!(parks_in(&log), vec![tile], "the old window parked the tile");
    assert!(
        !s1.read_with(&vcx1, |s, _| s.occupants.contains_key(&tile)),
        "the old window dropped the occupant"
    );
    let (s2, vcx2) = context_for(cx, &app, k2);
    assert!(
        s2.read_with(&vcx2, |s, _| s.occupants.contains_key(&tile)),
        "the new window rebuilt the occupant"
    );
    assert_eq!(app.read_with(&vcx2, |a, _| a.windows().len()), 2);
}

/// With nine windows showing every index, both verbs refuse with
/// `NoFreeWorkspace` and change nothing: every window still shows what it
/// showed, the registry still lists nine.
#[gpui::test]
fn both_verbs_refuse_with_nine_shown(cx: &mut gpui::TestAppContext) {
    let (services, log) = services_with_recorder();
    let app = test_app(cx, services, None);
    let (w1, mut vcx1) = open_shell_on(cx, app.clone());
    let s1 = shell_of(&w1, &mut vcx1);
    let k1 = s1.read_with(&vcx1, |s, _| s.key());
    dispatch_and_draw(&s1, &mut vcx1, "tile::add_rec");
    for _ in 0..8 {
        cx.update(|cx| ShellApp::open_fresh(&app, k1, cx))
            .expect("an index is free");
    }
    let before: Vec<(WindowKey, WorkspaceIx)> =
        app.read_with(&vcx1, |a, _| a.services().workspaces.windows().collect());
    assert_eq!(before.len(), 9, "sanity: nine windows show nine indexes");

    assert_eq!(
        vcx1.update(|_, cx| ShellApp::open_fresh(&app, k1, cx)),
        Err(OpenRefusal::NoFreeWorkspace)
    );
    assert_eq!(
        vcx1.update(|_, cx| ShellApp::tear_out(&app, k1, cx)),
        Err(OpenRefusal::NoFreeWorkspace)
    );
    vcx1.run_until_parked();

    let after: Vec<(WindowKey, WorkspaceIx)> =
        app.read_with(&vcx1, |a, _| a.services().workspaces.windows().collect());
    assert_eq!(after, before, "nothing changed");
    assert_eq!(app.read_with(&vcx1, |a, _| a.windows().len()), 9);
    assert!(
        parks_in(&log).is_empty(),
        "nothing was parked: {:?}",
        log.borrow()
    );
    assert_eq!(
        app.read_with(&vcx1, |a, _| a.services().workspaces.switch_epoch(k1)),
        0,
        "the first window never switched"
    );
}

/// `window_closed` parks every index the window hosted (shown or hidden)
/// into the app's parked records, unhosts them and forgets the window: the
/// registry drops it and no window hosts its indexes.
#[gpui::test]
fn window_closed_parks_and_forgets(cx: &mut gpui::TestAppContext) {
    let (services, log) = services_with_recorder();
    let app = test_app(cx, services, None);
    let (_w1, vcx1) = open_shell_on(cx, app.clone());
    let (w2, mut vcx2) = open_shell_on(cx, app.clone());
    let s2 = shell_of(&w2, &mut vcx2);
    let k2 = s2.read_with(&vcx2, |s, _| s.key());
    // The second window shows 2 and hosts 3 hidden, each with a tile.
    dispatch_and_draw(&s2, &mut vcx2, "workspace::switch_3");
    dispatch_and_draw(&s2, &mut vcx2, "tile::add_rec");
    let hidden = s2.read_with(&vcx2, |s, cx| s.workspace(cx).focused_tile().unwrap());
    dispatch_and_draw(&s2, &mut vcx2, "workspace::switch_2");
    dispatch_and_draw(&s2, &mut vcx2, "tile::add_rec");
    let shown_tile = s2.read_with(&vcx2, |s, cx| s.workspace(cx).focused_tile().unwrap());
    assert_eq!(app.read_with(&vcx2, |a, _| a.windows().len()), 2, "sanity");

    // While the window still stands, as the should-close hook runs.
    app.update(&mut vcx2, |a, cx| a.window_closed(k2, cx));
    vcx2.run_until_parked();

    assert_eq!(app.read_with(&vcx1, |a, _| a.windows().len()), 1);
    assert!(app.read_with(&vcx1, |a, _| a.handle(k2).is_none()));
    assert_eq!(app.read_with(&vcx1, |a, _| a.views().count()), 1);
    let two = WorkspaceIx::new(2).unwrap();
    let three = WorkspaceIx::new(3).unwrap();
    assert_eq!(
        app.read_with(&vcx1, |a, _| a.services().workspaces.host_of(two)),
        None
    );
    assert_eq!(
        app.read_with(&vcx1, |a, _| a.services().workspaces.host_of(three)),
        None
    );
    assert_eq!(shown(&app, &vcx1, k2), None);
    let mut parked = parks_in(&log);
    parked.sort();
    let mut expected = vec![hidden, shown_tile];
    expected.sort();
    assert_eq!(parked, expected, "both tiles were parked");
    assert!(
        app.read_with(&vcx1, |a, _| a.parked_tiles.contains_key(&hidden.0)
            && a.parked_tiles.contains_key(&shown_tile.0)),
        "the records wait in parked_tiles"
    );
    assert!(!s2.read_with(&vcx2, |s, _| s.occupants.contains_key(&shown_tile)));
    // The window can now go; the registry already forgot it.
    vcx2.update(|window, _| window.remove_window());
    vcx1.run_until_parked();
    assert_eq!(app.read_with(&vcx1, |a, _| a.windows().len()), 1);
}

/// The title is `Geode` alone with one registered window and names the
/// shown index with two or more; closing back to one drops the index.
#[gpui::test]
fn window_title_names_the_index_only_with_two_windows(cx: &mut gpui::TestAppContext) {
    let app = test_app(cx, test_services(), None);
    let (w1, mut vcx1) = open_shell_on(cx, app.clone());
    let k1 = shell_of(&w1, &mut vcx1).read_with(&vcx1, |s, _| s.key());
    assert_eq!(app.read_with(&vcx1, |a, _| a.window_title(k1)), "Geode");

    let k2 = vcx1
        .update(|_, cx| ShellApp::open_fresh(&app, k1, cx))
        .unwrap();
    assert_eq!(app.read_with(&vcx1, |a, _| a.window_title(k1)), "Geode · 1");
    assert_eq!(app.read_with(&vcx1, |a, _| a.window_title(k2)), "Geode · 2");

    app.update(&mut vcx1, |a, cx| a.window_closed(k2, cx));
    assert_eq!(app.read_with(&vcx1, |a, _| a.window_title(k1)), "Geode");
}

/// An opener that opens nothing rolls a tear-out back: the window keeps
/// showing its workspace, its occupant stays, nothing is parked (the park
/// the reservation queued finds the window showing the workspace again)
/// and the registry still lists one window.
#[gpui::test]
fn a_failing_opener_rolls_the_tear_out_back(cx: &mut gpui::TestAppContext) {
    let (services, log) = services_with_recorder();
    let app = test_app(cx, services, None);
    let (w1, mut vcx1) = open_shell_on(cx, app.clone());
    let s1 = shell_of(&w1, &mut vcx1);
    let k1 = s1.read_with(&vcx1, |s, _| s.key());
    dispatch_and_draw(&s1, &mut vcx1, "tile::add_rec");
    let tile = s1.read_with(&vcx1, |s, cx| s.workspace(cx).focused_tile().unwrap());

    let failing: WindowOpener = std::rc::Rc::new(|_, _, _| None);
    app.update(&mut vcx1, |a, _| a.set_window_opener(failing));

    assert_eq!(
        vcx1.update(|_, cx| ShellApp::tear_out(&app, k1, cx)),
        Err(OpenRefusal::OpenerFailed)
    );
    vcx1.run_until_parked();
    vcx1.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert_eq!(shown(&app, &vcx1, k1), Some(1), "the window still shows 1");
    assert_eq!(
        app.read_with(&vcx1, |a, _| a
            .services()
            .workspaces
            .host_of(WorkspaceIx::new(1).unwrap())),
        Some(k1)
    );
    assert!(
        s1.read_with(&vcx1, |s, _| s.occupants.contains_key(&tile)),
        "the tile is still an occupant"
    );
    assert!(
        parks_in(&log).is_empty(),
        "nothing was parked: {:?}",
        log.borrow()
    );
    assert!(app.read_with(&vcx1, |a, _| a.parked_tiles.is_empty()));
    assert_eq!(app.read_with(&vcx1, |a, _| a.windows().len()), 1);
    assert!(app.read_with(&vcx1, |a, _| a.is_last_window(k1)));
}

/// With no opener installed, `open_window` refuses `NoOpener` and reserves
/// nothing: the index stays free and the registry is empty.
#[gpui::test]
fn open_window_refuses_without_an_opener(cx: &mut gpui::TestAppContext) {
    use crate::shell::app::{Placement, WindowSpec};
    let app = test_app(cx, test_services(), None);
    let spec = WindowSpec {
        workspace: WorkspaceIx::new(1).unwrap(),
        placement: Placement::Default,
    };
    assert_eq!(
        cx.update(|cx| ShellApp::open_window(&app, spec, cx)),
        Err(OpenRefusal::NoOpener)
    );
    cx.update(|cx| {
        let a = app.read(cx);
        assert_eq!(
            a.services()
                .workspaces
                .host_of(WorkspaceIx::new(1).unwrap()),
            None
        );
        assert!(a.services().workspaces.windows().next().is_none());
        assert!(a.windows().is_empty());
    });
}
