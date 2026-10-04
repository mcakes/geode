//! The page seam from the shell's side: toggle, close, context stack,
//! visibility announcements, and the insert-mode route for page inputs.

use super::{
    dispatch_action, open_shell, services_with_page, services_with_recorders, shell_of, with_page,
    with_pages,
};
use crate::actions::ActionId;
use crate::module::recording::{PageRecorded, RecordingPageFactory};
use crate::shell::ShellView;
use crate::shell::input::{CLOSE_DIALOG_FIRST, CLOSE_PAGE_FIRST};

/// Whether the open page's own focus handle holds the keyboard.
fn page_focused(shell: &gpui::Entity<ShellView>, cx: &mut gpui::VisualTestContext) -> bool {
    cx.update(|window, cx| {
        let s = shell.read(cx);
        s.page
            .as_ref()
            .expect("a page was created")
            .occupant
            .content
            .focus_handle(cx)
            .is_focused(window)
    })
}

#[gpui::test]
fn toggle_opens_then_closes_the_page_and_announces_visibility(cx: &mut gpui::TestAppContext) {
    let factory = RecordingPageFactory::new("diagnostics");
    let log = factory.log();
    let (window, mut cx) = open_shell(cx, services_with_page(factory));
    let shell = shell_of(&window, &mut cx);
    assert!(!shell.read_with(&cx, |s, _| s.page_open()));

    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    assert!(shell.read_with(&cx, |s, _| s.page_open()));
    assert_eq!(
        shell.read_with(&cx, |s, _| s.open_page_kind()),
        Some("diagnostics")
    );
    assert_eq!(
        *log.borrow(),
        vec![PageRecorded::Created, PageRecorded::Visible(true)]
    );
    // The page view holds focus after open.
    assert!(page_focused(&shell, &mut cx));

    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    assert!(!shell.read_with(&cx, |s, _| s.page_open()));
    assert_eq!(log.borrow().last(), Some(&PageRecorded::Visible(false)));
    assert!(cx.update(|window, cx| shell.read(cx).focus_handle.is_focused(window)));
    // Retained: a second open does not create again.
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    assert_eq!(
        log.borrow()
            .iter()
            .filter(|r| **r == PageRecorded::Created)
            .count(),
        1
    );
}

#[gpui::test]
fn the_context_stack_is_page_then_kind_while_open(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(
        cx,
        services_with_page(RecordingPageFactory::new("diagnostics")),
    );
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    shell.read_with(&cx, |s, cx| {
        let stack = s.context_stack(cx);
        assert_eq!(stack.len(), 2, "{stack:?}");
        assert!(stack[0].has_flag("page"));
        assert!(stack[1].has_flag("diagnostics"));
        assert!(
            !stack
                .iter()
                .any(|c| c.has_flag("workspace") || c.has_flag("tile"))
        );
    });
}

#[gpui::test]
fn escape_closes_the_page_through_page_close(cx: &mut gpui::TestAppContext) {
    let factory = RecordingPageFactory::new("diagnostics");
    let log = factory.log();
    let (window, mut cx) = open_shell(cx, services_with_page(factory));
    let shell = shell_of(&window, &mut cx);
    cx.simulate_keystrokes("alt-d");
    assert!(
        shell.read_with(&cx, |s, _| s.page_open()),
        "mod+d opens via the toggle fragment"
    );
    cx.simulate_keystrokes("escape");
    assert!(!shell.read_with(&cx, |s, _| s.page_open()));
    assert!(
        log.borrow()
            .contains(&PageRecorded::Action("page::close".into())),
        "the page saw page::close before the shell acted"
    );
}

#[gpui::test]
fn a_page_that_consumes_close_stays_open(cx: &mut gpui::TestAppContext) {
    let factory = RecordingPageFactory::new("diagnostics");
    let consume = factory.consume_next_close();
    let (window, mut cx) = open_shell(cx, services_with_page(factory));
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    consume.set(true);
    cx.simulate_keystrokes("escape");
    assert!(
        shell.read_with(&cx, |s, _| s.page_open()),
        "consumed: still open"
    );
    cx.simulate_keystrokes("escape");
    assert!(
        !shell.read_with(&cx, |s, _| s.page_open()),
        "second escape closes"
    );
}

#[gpui::test]
fn toggle_under_a_modal_is_refused(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(
        cx,
        services_with_page(RecordingPageFactory::new("diagnostics")),
    );
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "settings::open", &mut cx);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    shell.read_with(&cx, |s, _| {
        assert!(s.modal_open());
        assert!(!s.page_open());
        assert_eq!(s.notice.as_deref(), Some(CLOSE_DIALOG_FIRST));
    });
}

/// `page::close` under a modal is refused exactly as the toggle is: the
/// palette can reach the id over the dialog stack, and closing the page
/// beneath a dialog would leave the dialog over a workspace it was not
/// opened from.
#[gpui::test]
fn page_close_under_a_modal_is_refused(cx: &mut gpui::TestAppContext) {
    let factory = RecordingPageFactory::new("diagnostics");
    let log = factory.log();
    let (window, mut cx) = open_shell(cx, services_with_page(factory));
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    dispatch_action(&shell, "settings::open", &mut cx);
    dispatch_action(&shell, "page::close", &mut cx);
    shell.read_with(&cx, |s, _| {
        assert!(s.modal_open());
        assert!(s.page_open(), "refused: still open");
        assert_eq!(s.notice.as_deref(), Some(CLOSE_DIALOG_FIRST));
    });
    assert!(
        !log.borrow()
            .contains(&PageRecorded::Action("page::close".into())),
        "the page never saw the refused close"
    );
}

#[gpui::test]
fn escape_under_a_modal_closes_the_modal_not_the_page(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(
        cx,
        services_with_page(RecordingPageFactory::new("diagnostics")),
    );
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    cx.simulate_keystrokes("ctrl-,");
    assert!(shell.read_with(&cx, |s, _| s.modal_open()));
    cx.simulate_keystrokes("escape");
    shell.read_with(&cx, |s, _| {
        assert!(!s.modal_open());
        assert!(s.page_open(), "the modal took the escape");
    });
    assert!(
        page_focused(&shell, &mut cx),
        "the emptied modal stack returns focus to the page, not the shell root"
    );
}

/// Closing the palette over an open page returns focus to the page's
/// handle. Left on the shell root, the page's own bindings would be
/// unreachable until the trader clicked into it.
#[gpui::test]
fn closing_the_palette_over_a_page_returns_focus_to_the_page(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(
        cx,
        services_with_page(RecordingPageFactory::new("diagnostics")),
    );
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    cx.simulate_keystrokes("ctrl-k");
    assert!(shell.read_with(&cx, |s, _| s.palette.is_some()));
    assert!(!page_focused(&shell, &mut cx), "the palette input took it");
    cx.simulate_keystrokes("escape");
    shell.read_with(&cx, |s, _| {
        assert!(s.palette.is_none());
        assert!(s.page_open());
    });
    assert!(page_focused(&shell, &mut cx));
}

/// A page may invoke its `ShellActions` handle from inside its own entity
/// update: the shell defers the dispatch, so `page::close` reaches the page's
/// `dispatch` (which reads that same entity) only after the update returns.
#[gpui::test]
fn a_page_may_close_itself_through_shell_actions_from_inside_its_own_update(
    cx: &mut gpui::TestAppContext,
) {
    let factory = RecordingPageFactory::new("diagnostics");
    let view = factory.view();
    let actions = factory.actions();
    let log = factory.log();
    let (window, mut cx) = open_shell(cx, services_with_page(factory));
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    let view = view.borrow().clone().expect("created");
    let actions = actions.borrow().clone().expect("created");
    cx.update(|window, cx| {
        view.update(cx, |_view, cx| {
            actions(&ActionId("page::close".into()), window, cx);
        });
    });
    cx.run_until_parked();
    assert!(!shell.read_with(&cx, |s, _| s.page_open()));
    assert!(
        log.borrow()
            .contains(&PageRecorded::Action("page::close".into())),
        "the page saw the close it asked for"
    );
}

#[gpui::test]
fn a_workspace_switch_closes_the_page(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(
        cx,
        services_with_page(RecordingPageFactory::new("diagnostics")),
    );
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    cx.simulate_keystrokes("alt-2");
    shell.read_with(&cx, |s, _| {
        assert!(!s.page_open());
        assert_eq!(s.services.workspaces.active_index(), 2);
    });
}

#[gpui::test]
fn a_workspace_switch_chord_from_a_focused_page_input_closes_the_page(
    cx: &mut gpui::TestAppContext,
) {
    let factory = RecordingPageFactory::new("diagnostics");
    let input = factory.input();
    let (window, mut cx) = open_shell(cx, services_with_page(factory));
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    let input = input.borrow().clone().expect("created");
    cx.update(|window, cx| input.update(cx, |i, cx| i.focus(window, cx)));
    // A bare key types into the input rather than reaching the keymap.
    cx.simulate_keystrokes("j");
    assert_eq!(cx.update(|_, cx| input.read(cx).value().to_string()), "j");
    cx.simulate_keystrokes("alt-3");
    shell.read_with(&cx, |s, _| {
        assert!(!s.page_open(), "the chord resolved against the whole stack");
        assert_eq!(s.services.workspaces.active_index(), 3);
    });
}

/// Every layout edit is refused over a page, not only the adds: the
/// palette reaches `workspace::close_tile` over a page, and a tile closed
/// unseen has no undo. A workspace switch stays the route home.
#[gpui::test]
fn layout_edits_are_refused_while_a_page_is_open(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(
        cx,
        services_with_page(RecordingPageFactory::new("diagnostics")),
    );
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "tile::add_rec", &mut cx);
    let tiles = |shell: &gpui::Entity<ShellView>, cx: &gpui::VisualTestContext| {
        shell.read_with(cx, |s, _| {
            s.services.workspaces.active().tree().tiles().len()
        })
    };
    assert_eq!(tiles(&shell, &cx), 1);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    cx.simulate_keystrokes("alt-n");
    shell.read_with(&cx, |s, _| {
        assert!(
            !s.modal_open(),
            "tile::add's picker did not open: refused over the page"
        );
        assert!(s.page_open());
        assert_eq!(s.notice.as_deref(), Some(CLOSE_PAGE_FIRST));
    });
    dispatch_action(&shell, "workspace::close_tile", &mut cx);
    shell.read_with(&cx, |s, _| {
        assert!(s.page_open());
        assert_eq!(s.notice.as_deref(), Some(CLOSE_PAGE_FIRST));
    });
    assert_eq!(tiles(&shell, &cx), 1, "the unseen tile still exists");
    dispatch_action(&shell, "dock::toggle_left", &mut cx);
    shell.read_with(&cx, |s, _| {
        assert!(s.page_open());
        assert_eq!(s.notice.as_deref(), Some(CLOSE_PAGE_FIRST));
        assert!(
            !s.services
                .workspaces
                .active()
                .docks()
                .get(crate::tiling::DockSide::Left)
                .visible(),
            "the dock did not open behind the page"
        );
    });
    // Refused, not routed to the page either.
    dispatch_action(&shell, "stack::next", &mut cx);
    shell.read_with(&cx, |s, _| {
        assert_eq!(s.notice.as_deref(), Some(CLOSE_PAGE_FIRST))
    });
    // The switch is exempt: it closes the page and switches.
    dispatch_action(&shell, "workspace::switch_2", &mut cx);
    shell.read_with(&cx, |s, _| {
        assert!(!s.page_open());
        assert_eq!(s.services.workspaces.active_index(), 2);
    });
}

/// A page's context carries `mode == insert` while its input holds focus,
/// so a `mode == normal` binding stays out of the insert route's bare-key
/// resolution: `j` types. Blurred back to the page handle, the same `j`
/// dispatches.
#[gpui::test]
fn a_mode_normal_page_binding_types_in_the_input_and_fires_after_blur(
    cx: &mut gpui::TestAppContext,
) {
    let factory = RecordingPageFactory::new("diagnostics");
    let input = factory.input();
    let log = factory.log();
    let (window, mut cx) = open_shell(cx, services_with_page(factory));
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    let input = input.borrow().clone().expect("created");
    // Focus-in and focus-out reach their listeners only in an active
    // window, with the next frame; the test platform activates on its
    // executor, so it is parked before the first focus move.
    cx.update(|window, _cx| window.activate_window());
    cx.run_until_parked();
    let draw = |cx: &mut gpui::VisualTestContext| {
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    };
    cx.update(|window, cx| input.update(cx, |i, cx| i.focus(window, cx)));
    draw(&mut cx);
    shell.read_with(&cx, |s, cx| {
        let stack = s.context_stack(cx);
        assert_eq!(stack[1].get("mode"), Some("insert"), "{stack:?}");
    });
    cx.simulate_keystrokes("j");
    assert_eq!(cx.update(|_, cx| input.read(cx).value().to_string()), "j");
    assert!(
        !log.borrow()
            .contains(&PageRecorded::Action("diagnostics::noop".into())),
        "a mode == normal binding does not fire in insert mode: {:?}",
        log.borrow()
    );
    cx.update(|window, cx| {
        shell
            .read(cx)
            .page
            .as_ref()
            .unwrap()
            .occupant
            .content
            .focus_handle(cx)
            .focus(window, cx);
    });
    draw(&mut cx);
    shell.read_with(&cx, |s, cx| {
        let stack = s.context_stack(cx);
        assert_eq!(stack[1].get("mode"), Some("normal"), "{stack:?}");
    });
    cx.simulate_keystrokes("j");
    assert!(
        log.borrow()
            .contains(&PageRecorded::Action("diagnostics::noop".into())),
        "blurred: the binding fires: {:?}",
        log.borrow()
    );
    assert_eq!(
        cx.update(|_, cx| input.read(cx).value().to_string()),
        "j",
        "and nothing typed"
    );
}

/// The page takes the tile surface alone: it starts beside the sidebar at
/// the toolbar's bottom edge and reaches the status bar. The toolbar is the
/// window's title bar and stays painted above it.
#[gpui::test]
fn the_page_paints_below_the_toolbar_above_the_status_bar(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(
        cx,
        services_with_page(RecordingPageFactory::new("diagnostics")),
    );
    let shell = shell_of(&window, &mut cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("shell-page").is_none());
    assert!(cx.debug_bounds("shell-sidebar").is_some());
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let page = cx.debug_bounds("shell-page").expect("page painted");
    let sidebar = cx.debug_bounds("shell-sidebar").expect("sidebar stays");
    let status = cx
        .debug_bounds("shell-status-bar")
        .expect("status bar stays");
    assert!(
        cx.debug_bounds("frame-readout").is_some(),
        "the toolbar stays painted over a page"
    );
    assert!(cx.debug_bounds("scope-field").is_some());
    assert_eq!(page.origin.x, sidebar.origin.x + sidebar.size.width);
    assert_eq!(page.origin.y, gpui_component::TITLE_BAR_HEIGHT);
    assert_eq!(
        page.origin.y + page.size.height,
        status.origin.y,
        "the page reaches the status bar and no further"
    );
    // A page taller than the surface pushes the status bar down with it,
    // so the edge above holds either way: pin the status bar to the
    // window's bottom edge too.
    let viewport = cx.update(|window, _| window.viewport_size());
    assert_eq!(status.origin.y + status.size.height, viewport.height);
}

/// A historical frame paints its warning stripe over a page too, and the
/// page starts below it: the stripe row belongs to the toolbar, not the page.
#[gpui::test]
fn a_historical_frame_keeps_its_stripe_above_the_page(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(
        cx,
        services_with_page(RecordingPageFactory::new("diagnostics")),
    );
    let shell = shell_of(&window, &mut cx);
    let frame = shell.read_with(&cx, |s, _| s.frame().clone());
    let at = chrono::Utc::now() - chrono::Duration::days(1);
    frame.update(&mut cx, |f, cx| {
        if f.shared_mut().set_as_of(geode_core::query::AsOf::At(at)) {
            cx.notify();
        }
    });
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let stripe = cx
        .debug_bounds("as-of-stripe")
        .expect("the stripe stays over a page");
    let page = cx.debug_bounds("shell-page").expect("page painted");
    let status = cx
        .debug_bounds("shell-status-bar")
        .expect("status bar stays");
    assert_eq!(stripe.origin.y, gpui_component::TITLE_BAR_HEIGHT);
    assert_eq!(page.origin.y, stripe.origin.y + stripe.size.height);
    assert_eq!(page.origin.y + page.size.height, status.origin.y);
    let viewport = cx.update(|window, _| window.viewport_size());
    assert_eq!(status.origin.y + status.size.height, viewport.height);
}

/// Open the page, focus the scope bar's text field through its registered
/// action, and type `abc` into it. The window is activated first: the
/// field's `Focus` event, which records the entry text, fires only from an
/// active window's draw.
fn type_in_the_scope_field_over_a_page(
    shell: &gpui::Entity<ShellView>,
    cx: &mut gpui::VisualTestContext,
) {
    cx.update(|window, _cx| window.activate_window());
    cx.run_until_parked();
    dispatch_action(shell, "page::toggle_diagnostics", cx);
    dispatch_action(shell, "frame::focus_text", cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(super::filter_is_focused(shell, cx));
    cx.simulate_input("abc");
    assert_eq!(
        shell.read_with(cx, |s, cx| s.filter_input.read(cx).value().to_string()),
        "abc"
    );
}

#[gpui::test]
fn escape_in_the_scope_input_over_a_page_returns_focus_to_the_page(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(
        cx,
        services_with_page(RecordingPageFactory::new("diagnostics")),
    );
    let shell = shell_of(&window, &mut cx);
    type_in_the_scope_field_over_a_page(&shell, &mut cx);
    cx.simulate_keystrokes("escape");
    assert_eq!(
        shell.read_with(&cx, |s, cx| s.filter_input.read(cx).value().to_string()),
        "",
        "escape restored the entry text"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.page_open()),
        "the field took the escape"
    );
    assert!(
        page_focused(&shell, &mut cx),
        "focus returns to the page, where its bindings are reachable"
    );
}

#[gpui::test]
fn enter_in_the_scope_input_over_a_page_returns_focus_to_the_page(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(
        cx,
        services_with_page(RecordingPageFactory::new("diagnostics")),
    );
    let shell = shell_of(&window, &mut cx);
    type_in_the_scope_field_over_a_page(&shell, &mut cx);
    cx.simulate_keystrokes("enter");
    assert_eq!(
        shell.read_with(&cx, |s, cx| s.filter_input.read(cx).value().to_string()),
        "abc",
        "enter keeps the typed text"
    );
    assert!(shell.read_with(&cx, |s, _| s.page_open()));
    assert!(page_focused(&shell, &mut cx));
}

/// Chords from the scope field resolve against `[workspace]` whether or not
/// a page is open: `mod+d` (the fixture's `alt-d`) closes the page.
#[gpui::test]
fn a_frame_chord_from_the_scope_input_over_a_page_still_resolves(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(
        cx,
        services_with_page(RecordingPageFactory::new("diagnostics")),
    );
    let shell = shell_of(&window, &mut cx);
    type_in_the_scope_field_over_a_page(&shell, &mut cx);
    cx.simulate_keystrokes("alt-d");
    assert!(!shell.read_with(&cx, |s, _| s.page_open()));
    assert_eq!(
        shell.read_with(&cx, |s, cx| s.filter_input.read(cx).value().to_string()),
        "abc",
        "the chord typed nothing"
    );
}

/// Opening a page hides every tile beneath it (each hears `set_visible(false)`
/// and no flip barrier waits on one); closing shows them again.
#[gpui::test]
fn tiles_beneath_are_hidden_on_open_and_shown_on_close(cx: &mut gpui::TestAppContext) {
    use crate::module::recording::{Recorded, RecordingFactory};
    // `rec`, not another kind: the fixture keymap binds `tile::add_rec_*`.
    let recorder = RecordingFactory::new("rec");
    let tile_log = recorder.log.clone();
    let services = with_page(
        services_with_recorders(vec![recorder]),
        RecordingPageFactory::new("diagnostics"),
    );
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "tile::add_rec", &mut cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        tile_log
            .borrow()
            .iter()
            .any(|r| matches!(r, Recorded::Visible(_, true)))
    );
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    // Focus is reported after visibility, so the last visibility entry is
    // the one that says the tile went under the page.
    let last_visible = |log: &[Recorded]| {
        log.iter()
            .rev()
            .find(|r| matches!(r, Recorded::Visible(..)))
            .cloned()
    };
    assert!(
        matches!(
            last_visible(&tile_log.borrow()),
            Some(Recorded::Visible(_, false))
        ),
        "hidden beneath the page: {:?}",
        tile_log.borrow()
    );
    // The tiles leaving the screen must not pull focus off the page: the
    // render's "a tile left the screen" net is for tiles, not the page.
    assert!(
        page_focused(&shell, &mut cx),
        "the page keeps focus through the render that hides the tiles"
    );
    let keys = shell.read_with(&cx, |s, _| {
        let mut v = Vec::new();
        s.visible_tile_keys(&mut v);
        v
    });
    assert!(keys.is_empty(), "no visible tile keys while a page is open");
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(matches!(
        last_visible(&tile_log.borrow()),
        Some(Recorded::Visible(_, true))
    ));
}

/// One sidebar button per registered page, above the settings avatar. A
/// click toggles the page through `page::toggle_<kind>`; the open page's
/// button takes the active-tab treatment; a mouse-opened page then hears
/// keys (escape closes it).
#[gpui::test]
fn the_sidebar_button_toggles_the_page_and_shows_it_active(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(
        cx,
        services_with_page(RecordingPageFactory::new("diagnostics")),
    );
    let shell = shell_of(&window, &mut cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let button = cx
        .debug_bounds("sidebar-page-diagnostics")
        .expect("one button per page");
    let profile = cx.debug_bounds("sidebar-profile").unwrap();
    assert!(
        button.origin.y < profile.origin.y,
        "above the settings avatar"
    );
    assert!(cx.debug_bounds("sidebar-page-diagnostics-active").is_none());
    cx.simulate_mouse_down(
        button.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_up(
        button.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(shell.read_with(&cx, |s, _| s.page_open()));
    assert!(
        cx.debug_bounds("sidebar-page-diagnostics-active").is_some(),
        "active treatment"
    );
    // A mouse-opened page must then receive keys: escape closes it.
    cx.simulate_keystrokes("escape");
    assert!(!shell.read_with(&cx, |s, _| s.page_open()));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("sidebar-page-diagnostics-active").is_none());
}

/// The sidebar button's tooltip names the page and its toggle chord.
#[gpui::test]
fn hovering_the_sidebar_page_button_names_the_page_and_its_chord(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(
        cx,
        services_with_page(RecordingPageFactory::new("diagnostics")),
    );
    let _shell = shell_of(&window, &mut cx);
    let button = cx
        .debug_bounds("sidebar-page-diagnostics")
        .expect("button painted");
    assert!(cx.debug_bounds("tip-sidebar-page-diagnostics").is_none());
    cx.simulate_mouse_move(
        button.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(600));
    cx.run_until_parked();
    assert!(cx.debug_bounds("tip-sidebar-page-diagnostics").is_some());
    assert!(
        cx.debug_bounds("tip-sidebar-page-diagnostics-chord-alt+d")
            .is_some()
            || cx
                .debug_bounds("tip-sidebar-page-diagnostics-chord-mod+d")
                .is_some(),
        "the chord chip names the toggle binding"
    );
}

/// One `[pages.<kind>]` table with a single string field.
fn page_table(key: &str, value: &str) -> toml::Table {
    let mut t = toml::Table::new();
    t.insert(key.into(), toml::Value::String(value.into()));
    t
}

/// Point the flush at a path (it never writes; the snapshot is returned) and
/// force one flush so the baselines are clean.
fn arm_session_flush(shell: &gpui::Entity<ShellView>, cx: &mut gpui::VisualTestContext) -> String {
    shell
        .update(cx, |s, cx| {
            s.services.session_path = Some(std::path::PathBuf::from("/nonexistent/session.toml"));
            s.session_dirty = true;
            s.take_dirty_session_write(cx)
        })
        .expect("the layout flag forces a flush")
        .1
}

#[gpui::test]
fn a_restored_pages_table_reaches_the_factory_create(cx: &mut gpui::TestAppContext) {
    let factory = RecordingPageFactory::new("diagnostics");
    let restored = factory.restored();
    let mut services = services_with_page(factory);
    let table = page_table("section", "log");
    services
        .restored_pages
        .insert("diagnostics".into(), table.clone());
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    assert!(restored.borrow().is_none(), "not created yet");
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    assert_eq!(*restored.borrow(), Some(table));
    // Consumed: the live page's own state is what the next flush writes.
    shell.read_with(&cx, |s, _| assert!(s.services.restored_pages.is_empty()));
}

#[gpui::test]
fn a_page_state_change_alone_flushes_once_and_is_quiet_afterwards(cx: &mut gpui::TestAppContext) {
    let factory = RecordingPageFactory::new("diagnostics");
    let serialized = factory.serialized();
    let (window, mut cx) = open_shell(cx, services_with_page(factory));
    let shell = shell_of(&window, &mut cx);
    // Nothing created yet: no pages table.
    let text = arm_session_flush(&shell, &mut cx);
    assert!(!text.contains("[pages"), "no page created, nothing written");
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    let (_, text) = shell
        .update(&mut cx, |s, cx| s.take_dirty_session_write(cx))
        .unwrap();
    assert!(
        text.contains("[pages.diagnostics]") && text.contains("recorded = true"),
        "{text}"
    );
    assert!(
        shell
            .update(&mut cx, |s, cx| s.take_dirty_session_write(cx))
            .is_none(),
        "nothing changed: nothing written"
    );
    // The page's state moves without any shell action, so no layout flag
    // is set; the snapshot comparison alone must notice.
    serialized
        .borrow_mut()
        .insert("section".into(), toml::Value::String("log".into()));
    let (_, text) = shell
        .update(&mut cx, |s, cx| {
            assert!(!s.session_dirty);
            s.take_dirty_session_write(cx)
        })
        .expect("a page-state-only change flushes");
    assert!(text.contains("section = \"log\""), "{text}");
    assert!(
        shell
            .update(&mut cx, |s, cx| s.take_dirty_session_write(cx))
            .is_none(),
        "written once, then quiet"
    );
}

#[gpui::test]
fn a_replaced_page_kind_keeps_its_state_through_the_next_flush(cx: &mut gpui::TestAppContext) {
    let diagnostics = RecordingPageFactory::new("diagnostics");
    let other = RecordingPageFactory::new("other").without_toggle_binding();
    let diagnostics_restored = diagnostics.restored();
    *diagnostics.serialized().borrow_mut() = page_table("which", "diagnostics");
    *other.serialized().borrow_mut() = page_table("which", "other");
    let services = with_pages(
        services_with_recorders(vec![crate::module::recording::RecordingFactory::new("rec")]),
        vec![diagnostics, other],
    );
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    let text = arm_session_flush(&shell, &mut cx);
    assert!(text.contains("[pages.diagnostics]"), "{text}");
    // A different kind replaces the retained page; its last state is kept.
    dispatch_action(&shell, "page::toggle_other", &mut cx);
    shell.read_with(&cx, |s, _| assert_eq!(s.open_page_kind(), Some("other")));
    let (_, text) = shell
        .update(&mut cx, |s, cx| s.take_dirty_session_write(cx))
        .unwrap();
    assert!(
        text.contains("[pages.diagnostics]") && text.contains("which = \"diagnostics\""),
        "the replaced kind's table survives: {text}"
    );
    assert!(
        text.contains("[pages.other]") && text.contains("which = \"other\""),
        "{text}"
    );
    // Reopening the replaced kind hands that kept table back to `create`
    // and consumes it; now `other` is the replaced one whose state is kept.
    assert!(
        diagnostics_restored.borrow().is_none(),
        "first create: nothing to restore"
    );
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    assert_eq!(
        *diagnostics_restored.borrow(),
        Some(page_table("which", "diagnostics"))
    );
    shell.read_with(&cx, |s, _| {
        assert_eq!(s.open_page_kind(), Some("diagnostics"));
        assert_eq!(
            s.services.restored_pages.keys().collect::<Vec<_>>(),
            vec!["other"]
        );
    });
}

#[gpui::test]
fn the_overlay_mirror_follows_a_keyboard_toggle(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(
        cx,
        services_with_page(RecordingPageFactory::new("diagnostics")),
    );
    let shell = shell_of(&window, &mut cx);
    let diagnostics = shell.read_with(&cx, |s, _| s.diagnostics().clone());
    assert!(!diagnostics.read_with(&cx, |d, _| d.overlay_visible()));
    dispatch_action(&shell, "perf::toggle_overlay", &mut cx);
    assert!(diagnostics.read_with(&cx, |d, _| d.overlay_visible()));
    // The entity channel toggles it back and the mirror follows.
    diagnostics.update(&mut cx, |d, cx| {
        d.request_overlay_toggle();
        cx.notify();
    });
    cx.run_until_parked();
    assert!(!diagnostics.read_with(&cx, |d, _| d.overlay_visible()));
    assert!(!shell.read_with(&cx, |s, _| s.perf_overlay));
}

/// A queued request from a tile's health chip opens the diagnostics page
/// through the shell's own diagnostics observer.
#[gpui::test]
fn a_queued_page_open_opens_the_diagnostics_page(cx: &mut gpui::TestAppContext) {
    let factory = RecordingPageFactory::new("diagnostics");
    let log = factory.log();
    let (window, mut cx) = open_shell(cx, services_with_page(factory));
    let shell = shell_of(&window, &mut cx);
    let diagnostics = shell.read_with(&cx, |s, _| s.diagnostics().clone());
    diagnostics.update(&mut cx, |d, cx| {
        d.request_diagnostics_page("risk_src");
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |s, _| s.open_page_kind()),
        Some("diagnostics")
    );
    assert!(log.borrow().contains(&PageRecorded::Visible(true)));
    assert_eq!(
        log.borrow().last(),
        Some(&PageRecorded::Reveal("risk_src".into())),
        "the opened page reveals the requested source"
    );
    assert_eq!(
        diagnostics.update(&mut cx, |d, _| d.take_pending_diagnostics_page()),
        None,
        "the shell drained the request"
    );
}

/// The request only ever opens: a second one while the page is open
/// leaves it open (a toggle would close it).
#[gpui::test]
fn a_queued_page_open_never_closes_the_page(cx: &mut gpui::TestAppContext) {
    let factory = RecordingPageFactory::new("diagnostics");
    let log = factory.log();
    let (window, mut cx) = open_shell(cx, services_with_page(factory));
    let shell = shell_of(&window, &mut cx);
    let diagnostics = shell.read_with(&cx, |s, _| s.diagnostics().clone());
    for _ in 0..2 {
        diagnostics.update(&mut cx, |d, cx| {
            d.request_diagnostics_page("risk_src");
            cx.notify();
        });
        cx.run_until_parked();
    }
    assert!(shell.read_with(&cx, |s, _| s.page_open()));
    assert!(!log.borrow().contains(&PageRecorded::Visible(false)));
    let reveals = log
        .borrow()
        .iter()
        .filter(|r| matches!(r, PageRecorded::Reveal(_)))
        .count();
    assert_eq!(reveals, 2, "an already-open page still reveals the source");
}

/// Under a modal the request is refused as the toggle is.
#[gpui::test]
fn a_queued_page_open_under_a_modal_is_refused(cx: &mut gpui::TestAppContext) {
    let factory = RecordingPageFactory::new("diagnostics");
    let log = factory.log();
    let (window, mut cx) = open_shell(cx, services_with_page(factory));
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "settings::open", &mut cx);
    let diagnostics = shell.read_with(&cx, |s, _| s.diagnostics().clone());
    diagnostics.update(&mut cx, |d, cx| {
        d.request_diagnostics_page("risk_src");
        cx.notify();
    });
    cx.run_until_parked();
    shell.read_with(&cx, |s, _| {
        assert!(s.modal_open());
        assert!(!s.page_open());
        assert_eq!(s.notice.as_deref(), Some(CLOSE_DIALOG_FIRST));
    });
    assert!(
        !log.borrow()
            .iter()
            .any(|r| matches!(r, PageRecorded::Reveal(_))),
        "a refused request reveals nothing"
    );
}

/// The chip's request begins as the page's action does: the last refusal
/// expires rather than staying over the page, and the crash tail names
/// `page::toggle_diagnostics`.
#[gpui::test]
fn a_queued_page_open_clears_the_notice_and_records_the_action(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(
        cx,
        services_with_page(RecordingPageFactory::new("diagnostics")),
    );
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    dispatch_action(&shell, "workspace::close_tile", &mut cx);
    shell.read_with(&cx, |s, _| {
        assert_eq!(
            s.notice.as_deref(),
            Some(CLOSE_PAGE_FIRST),
            "a refusal is showing"
        );
    });
    let diagnostics = shell.read_with(&cx, |s, _| s.diagnostics().clone());
    diagnostics.update(&mut cx, |d, cx| {
        d.request_diagnostics_page("risk_src");
        cx.notify();
    });
    cx.run_until_parked();
    shell.read_with(&cx, |s, _| {
        assert!(s.page_open());
        assert_eq!(s.notice, None, "the request expired the old refusal");
        let last = s.services.action_tail.lock().unwrap().recent().last();
        assert_eq!(
            last,
            Some(crate::diagnostics::fnv1a("page::toggle_diagnostics"))
        );
    });
}
