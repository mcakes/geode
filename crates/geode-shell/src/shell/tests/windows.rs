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
    assert!(
        app.read_with(&vcx1, |a, _| a.parked_tiles.is_empty()),
        "no record waits in parked_tiles"
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

/// `window::tear_out` is a registered palette action (Workspace category,
/// no chord), and dispatching it in a window opens a second window: the
/// registry lists two and the dispatching window fell back to the free
/// index.
#[gpui::test]
fn tear_out_is_a_registered_action_that_opens_a_window(cx: &mut gpui::TestAppContext) {
    let app = test_app(cx, test_services(), None);
    let (w1, mut vcx1) = open_shell_on(cx, app.clone());
    let s1 = shell_of(&w1, &mut vcx1);
    let k1 = s1.read_with(&vcx1, |s, _| s.key());
    for (id, title) in [
        ("window::tear_out", "Tear out into a new window"),
        ("window::open", "New window"),
    ] {
        let def = app.read_with(&vcx1, |a, _| {
            a.services()
                .registry
                .get(&ActionId(id.to_string()))
                .cloned()
        });
        let def = def.unwrap_or_else(|| panic!("{id} is registered"));
        assert_eq!(def.title, title);
        assert_eq!(def.category, "Workspace");
    }

    dispatch_and_draw(&s1, &mut vcx1, "window::tear_out");
    vcx1.run_until_parked();

    assert_eq!(app.read_with(&vcx1, |a, _| a.windows().len()), 2);
    assert_eq!(shown(&app, &vcx1, k1), Some(2), "the old window fell back");
    assert!(
        s1.read_with(&vcx1, |s, _| s.notice.is_none()),
        "no refusal notice"
    );
}

/// Under a modal both window verbs are refused with the close-the-dialog
/// notice, as a workspace switch is: no window opens, the dialog stays
/// open and the window still shows its index.
#[gpui::test]
fn open_is_refused_under_a_modal(cx: &mut gpui::TestAppContext) {
    let app = test_app(cx, test_services(), None);
    let (w1, mut vcx1) = open_shell_on(cx, app.clone());
    let s1 = shell_of(&w1, &mut vcx1);
    let k1 = s1.read_with(&vcx1, |s, _| s.key());
    dispatch_and_draw(&s1, &mut vcx1, "settings::open");
    assert!(s1.read_with(&vcx1, |s, _| s.modal_open()), "sanity");

    for id in ["window::open", "window::tear_out"] {
        dispatch_and_draw(&s1, &mut vcx1, id);
        vcx1.run_until_parked();
        assert_eq!(
            s1.read_with(&vcx1, |s, _| s.notice.clone()).as_deref(),
            Some(crate::shell::input::CLOSE_DIALOG_FIRST),
            "{id} is refused with the notice"
        );
        assert_eq!(
            app.read_with(&vcx1, |a, _| a.windows().len()),
            1,
            "{id} opened nothing"
        );
        assert!(
            s1.read_with(&vcx1, |s, _| s.modal_open()),
            "{id} left the dialog open"
        );
        assert_eq!(shown(&app, &vcx1, k1), Some(1), "{id} switched nothing");
    }
}

/// With nine windows showing every index, either verb dispatched in a
/// window shows the full-house notice and changes nothing.
#[gpui::test]
fn a_full_house_shows_the_notice(cx: &mut gpui::TestAppContext) {
    let app = test_app(cx, test_services(), None);
    let (w1, mut vcx1) = open_shell_on(cx, app.clone());
    let s1 = shell_of(&w1, &mut vcx1);
    let k1 = s1.read_with(&vcx1, |s, _| s.key());
    for _ in 0..8 {
        cx.update(|cx| ShellApp::open_fresh(&app, k1, cx))
            .expect("an index is free");
    }
    assert_eq!(app.read_with(&vcx1, |a, _| a.windows().len()), 9, "sanity");

    for id in ["window::tear_out", "window::open"] {
        dispatch_and_draw(&s1, &mut vcx1, id);
        vcx1.run_until_parked();
        assert_eq!(
            s1.read_with(&vcx1, |s, _| s.notice.clone()).as_deref(),
            Some("all nine workspaces are on screen"),
            "{id}"
        );
        assert_eq!(app.read_with(&vcx1, |a, _| a.windows().len()), 9, "{id}");
        assert_eq!(shown(&app, &vcx1, k1), Some(1), "{id}");
    }
}

/// The window's title follows the registry: `Geode` alone, `Geode · N`
/// once a second window opens (in both windows), the new index after a
/// switch, and `Geode` again when the count falls back to one. Read
/// through the view's last-set title, which the event and dispatch paths
/// set and render never touches.
#[gpui::test]
fn the_title_follows_the_window_count(cx: &mut gpui::TestAppContext) {
    let app = test_app(cx, test_services(), None);
    let (w1, mut vcx1) = open_shell_on(cx, app.clone());
    let s1 = shell_of(&w1, &mut vcx1);
    let k1 = s1.read_with(&vcx1, |s, _| s.key());
    let title = |s: &Entity<ShellView>, cx: &gpui::VisualTestContext| {
        s.read_with(cx, |s, _| s.last_title().map(str::to_owned))
    };
    assert_eq!(title(&s1, &vcx1).as_deref(), Some("Geode"));

    let k2 = vcx1
        .update(|_, cx| ShellApp::open_fresh(&app, k1, cx))
        .expect("a free index");
    vcx1.run_until_parked();
    assert_eq!(title(&s1, &vcx1).as_deref(), Some("Geode · 1"));
    let (s2, vcx2) = context_for(cx, &app, k2);
    assert_eq!(title(&s2, &vcx2).as_deref(), Some("Geode · 2"));

    // A switch in the first window renames it after the index it shows.
    dispatch_and_draw(&s1, &mut vcx1, "workspace::switch_3");
    assert_eq!(title(&s1, &vcx1).as_deref(), Some("Geode · 3"));

    app.update(&mut vcx1, |a, cx| a.window_closed(k2, cx));
    vcx1.run_until_parked();
    assert_eq!(title(&s1, &vcx1).as_deref(), Some("Geode"));
}

/// A tear-out switches the dispatching window to the lowest free index
/// inside the app, so the window must run its own post-switch work on
/// the verb's path: its flip baseline is re-seeded from the lane it now
/// shows (workspace 2's pinned lane, whose scope generation differs from
/// workspace 1's), and its scope field shows that lane's text.
#[gpui::test]
fn tear_out_reseeds_the_dispatching_window(cx: &mut gpui::TestAppContext) {
    use geode_core::scope::Scope;
    let app = test_app(cx, test_services(), None);
    let (w1, mut vcx1) = open_shell_on(cx, app.clone());
    let s1 = shell_of(&w1, &mut vcx1);
    let k1 = s1.read_with(&vcx1, |s, _| s.key());
    let frame = s1.read_with(&vcx1, |s, _| s.frame().clone());
    let two = WorkspaceIx::new(2).unwrap();
    frame.update(&mut vcx1, |f, _| {
        assert!(f.pin(two));
        assert!(f.view_mut(two).set_scope(Scope {
            text: Some("spx".to_string()),
            ..Scope::default()
        }));
    });
    let (one_v, two_v) = frame.read_with(&vcx1, |f, _| {
        (
            f.view(WorkspaceIx::FIRST).versions(),
            f.view(two).versions(),
        )
    });
    assert!(!one_v.same_flip_identity(two_v), "sanity: the lanes differ");
    assert!(
        s1.read_with(&vcx1, |s, _| s.last_flip_versions.same_flip_identity(one_v)),
        "sanity: the baseline is lane 1's"
    );

    dispatch_and_draw(&s1, &mut vcx1, "window::tear_out");
    vcx1.run_until_parked();

    assert_eq!(shown(&app, &vcx1, k1), Some(2), "the window fell back to 2");
    assert!(
        s1.read_with(&vcx1, |s, _| s.last_flip_versions.same_flip_identity(two_v)),
        "the flip baseline was re-seeded from lane 2"
    );
    let field = s1.read_with(&vcx1, |s, cx| s.filter_input.read(cx).value().to_string());
    assert_eq!(field, "spx", "the scope field shows lane 2's text");
    assert_eq!(app.read_with(&vcx1, |a, _| a.windows().len()), 2);
}

/// A keyed delivery for `tile`, in flight from the data thread.
fn query_for(tile: TileId, tag: u64) -> crate::module::Delivery {
    crate::module::Delivery::Query(geode_core::query::QueryOutcome {
        key: geode_core::query::QueryKey(tile.0),
        tag,
        snapshot: Err("no rows".into()),
        submitted: std::time::Instant::now(),
    })
}

fn delivered_in(log: &std::rc::Rc<std::cell::RefCell<Vec<Recorded>>>) -> Vec<(TileId, u64)> {
    log.borrow()
        .iter()
        .filter_map(|r| match r {
            Recorded::Delivered(t, tag) => Some((*t, *tag)),
            _ => None,
        })
        .collect()
}

/// A keyed answer goes to the window hosting the tile's workspace, shown
/// or hidden, and to no other: W1 hosts tile 1 hidden in workspace 2 and
/// W2 shows 3 with its own tile; the answer for tile 1 is routed to W1
/// alone and reaches tile 1 once.
#[gpui::test]
fn a_keyed_delivery_reaches_the_hosting_window_only(cx: &mut gpui::TestAppContext) {
    use crate::shell::app::{Placement, Route, WindowSpec};
    let (services, log) = services_with_recorder();
    let app = test_app(cx, services, None);
    let (w1, mut vcx1) = open_shell_on(cx, app.clone());
    let s1 = shell_of(&w1, &mut vcx1);
    let k1 = s1.read_with(&vcx1, |s, _| s.key());
    dispatch_and_draw(&s1, &mut vcx1, "workspace::switch_2");
    dispatch_and_draw(&s1, &mut vcx1, "tile::add_rec");
    let hidden = s1.read_with(&vcx1, |s, cx| s.workspace(cx).focused_tile().unwrap());
    dispatch_and_draw(&s1, &mut vcx1, "workspace::switch_1");
    assert_eq!(shown(&app, &vcx1, k1), Some(1), "sanity: 2 is hidden in W1");

    let k2 = cx
        .update(|cx| {
            ShellApp::open_window(
                &app,
                WindowSpec {
                    workspace: WorkspaceIx::new(3).unwrap(),
                    placement: Placement::Default,
                },
                cx,
            )
        })
        .expect("3 is free");
    let (s2, mut vcx2) = context_for(cx, &app, k2);
    dispatch_and_draw(&s2, &mut vcx2, "tile::add_rec");
    let other = s2.read_with(&vcx2, |s, cx| s.workspace(cx).focused_tile().unwrap());
    assert_ne!(hidden, other);
    log.borrow_mut().clear();

    let delivery = query_for(hidden, 7);
    assert_eq!(
        app.read_with(&vcx1, |a, _| a.route(&delivery)),
        Route::Window(k1),
        "the host of the tile's workspace, not every window"
    );
    cx.update(|cx| ShellApp::deliver(&app, delivery, cx));
    vcx1.run_until_parked();

    assert_eq!(
        delivered_in(&log),
        vec![(hidden, 7)],
        "tile 1 heard its answer once and tile 2 heard nothing: {:?}",
        log.borrow()
    );
}

/// A keyless delivery (a fetch finished) reaches every window: each
/// window's visible recorder tile hears the same pair.
#[gpui::test]
fn a_keyless_delivery_reaches_every_window(cx: &mut gpui::TestAppContext) {
    use crate::shell::app::Route;
    let (services, log) = services_with_recorder();
    let app = test_app(cx, services, None);
    let (w1, mut vcx1) = open_shell_on(cx, app.clone());
    let s1 = shell_of(&w1, &mut vcx1);
    let k1 = s1.read_with(&vcx1, |s, _| s.key());
    dispatch_and_draw(&s1, &mut vcx1, "tile::add_rec");
    let t1 = s1.read_with(&vcx1, |s, cx| s.workspace(cx).focused_tile().unwrap());
    let k2 = cx
        .update(|cx| ShellApp::open_fresh(&app, k1, cx))
        .expect("a free index");
    let (s2, mut vcx2) = context_for(cx, &app, k2);
    dispatch_and_draw(&s2, &mut vcx2, "tile::add_rec");
    let t2 = s2.read_with(&vcx2, |s, cx| s.workspace(cx).focused_tile().unwrap());
    log.borrow_mut().clear();

    let delivery = crate::module::Delivery::SeriesFetched {
        source: "demo_kdb".into(),
        identity: "VIX".into(),
        result: Ok(3),
    };
    assert_eq!(
        app.read_with(&vcx1, |a, _| a.route(&delivery)),
        Route::Broadcast
    );
    cx.update(|cx| ShellApp::deliver(&app, delivery, cx));
    vcx1.run_until_parked();

    let mut heard: Vec<TileId> = log
        .borrow()
        .iter()
        .filter_map(|r| match r {
            Recorded::SeriesFetched(t, pair) if pair == "VIX@demo_kdb" => Some(*t),
            _ => None,
        })
        .collect();
    heard.sort();
    let mut both = vec![t1, t2];
    both.sort();
    assert_eq!(
        heard,
        both,
        "each window's tile heard the fetch: {:?}",
        log.borrow()
    );
}

/// An answer for a tile whose workspace no window hosts (its window closed
/// and parked it) is dropped: no panic, no delivery, and the record still
/// waits in `parked_tiles`.
#[gpui::test]
fn an_answer_for_a_parked_tile_is_dropped(cx: &mut gpui::TestAppContext) {
    use crate::shell::app::Route;
    let (services, log) = services_with_recorder();
    let app = test_app(cx, services, None);
    let (w1, mut vcx1) = open_shell_on(cx, app.clone());
    let k1 = shell_of(&w1, &mut vcx1).read_with(&vcx1, |s, _| s.key());
    let k2 = cx
        .update(|cx| ShellApp::open_fresh(&app, k1, cx))
        .expect("a free index");
    let (s2, mut vcx2) = context_for(cx, &app, k2);
    dispatch_and_draw(&s2, &mut vcx2, "tile::add_rec");
    let tile = s2.read_with(&vcx2, |s, cx| s.workspace(cx).focused_tile().unwrap());
    app.update(&mut vcx2, |a, cx| a.window_closed(k2, cx));
    vcx2.run_until_parked();
    assert!(
        app.read_with(&vcx1, |a, _| a.parked_tiles.contains_key(&tile.0)),
        "sanity: the tile was parked"
    );
    log.borrow_mut().clear();

    let delivery = query_for(tile, 9);
    assert_eq!(app.read_with(&vcx1, |a, _| a.route(&delivery)), Route::Drop);
    cx.update(|cx| ShellApp::deliver(&app, delivery, cx));
    vcx1.run_until_parked();

    assert!(
        delivered_in(&log).is_empty(),
        "nothing heard the answer: {:?}",
        log.borrow()
    );
    assert!(
        app.read_with(&vcx1, |a, _| a.parked_tiles.contains_key(&tile.0)),
        "the record still waits for a window"
    );
    vcx2.update(|window, _| window.remove_window());
}

/// Picker tags come from the app, so two windows' pickers never share
/// one: a picker opened in each window carries a distinct tag, the later
/// open's the larger, and both came from the app's one counter.
#[gpui::test]
fn picker_tags_are_unique_across_windows(cx: &mut gpui::TestAppContext) {
    let app = test_app(cx, super::picker::services_with_pickable(), None);
    let (w1, mut vcx1) = open_shell_on(cx, app.clone());
    let s1 = shell_of(&w1, &mut vcx1);
    let k1 = s1.read_with(&vcx1, |s, _| s.key());
    let k2 = cx
        .update(|cx| ShellApp::open_fresh(&app, k1, cx))
        .expect("a free index");
    let (s2, mut vcx2) = context_for(cx, &app, k2);

    dispatch_action(&s1, "frame::pick_book", &mut vcx1);
    let first = s1.read_with(&vcx1, |s, _| s.picker.as_ref().expect("picker open").tag);
    dispatch_action(&s2, "frame::pick_book", &mut vcx2);
    let second = s2.read_with(&vcx2, |s, _| s.picker.as_ref().expect("picker open").tag);
    assert!(
        second > first,
        "the second window's open must not repeat the first's tag: {first} then {second}"
    );
    assert_eq!(
        app.update(&mut vcx1, |a, _| a.next_tag()),
        second + 1,
        "both opens drew from the app's one counter"
    );
}

/// The session's window records follow the registry: K is a window's
/// 1-based registration position and each record names the index that
/// window shows, whichever window is the one under update. The test
/// platform's one display is id 1, and every live window has bounds.
#[gpui::test]
fn window_records_follow_registration_order(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.toml");
    let mut services = test_services();
    services.session_path = Some(path.clone());
    let app = test_app(cx, services, None);
    let (_w1, _vcx1) = open_shell_on(cx, app.clone());
    let (_w2, _vcx2) = open_shell_on(cx, app.clone());
    let shown: Vec<u8> = cx.update(|cx| {
        app.read(cx)
            .windows()
            .iter()
            .map(|(key, _)| {
                app.read(cx)
                    .services()
                    .workspaces
                    .shown(*key)
                    .unwrap()
                    .get()
            })
            .collect()
    });
    assert_eq!(shown, vec![1, 2], "sanity: the first shows 1, the second 2");

    let records = cx.update(|cx| app.update(cx, |a, cx| a.window_records(None, cx)));
    let workspaces: Vec<u8> = records.iter().map(|r| r.workspace).collect();
    assert_eq!(workspaces, vec![1, 2], "K order is registration order");
    assert!(
        records.iter().all(|r| r.bounds.is_some()),
        "a live window has bounds: {records:?}"
    );
    assert_eq!(records[0].display, Some(1), "the test platform's display");
    assert!(!records[0].maximized);

    // The same order through the production save.
    cx.update(|cx| ShellApp::save_session(&app, None, cx));
    let restored = crate::session::load(&path);
    assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
    let saved: Vec<u8> = restored.windows.iter().map(|r| r.workspace).collect();
    assert_eq!(saved, vec![1, 2], "the file lists windows.1 then windows.2");
}
