//! The `/` and `:` command-line seam: opening, completion, accept,
//! and the cancel triggers that close it from elsewhere in the shell.

use super::*;

#[gpui::test]
fn colon_opens_the_command_line_and_enter_runs_the_line_on_the_occupant(
    cx: &mut gpui::TestAppContext,
) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes(":");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("command-line").is_some(),
        "the strip painted"
    );
    cx.simulate_input("unpin");
    cx.simulate_keystrokes("enter");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("command-line").is_none(),
        "closed after a successful command"
    );
    let shell = shell_of(&window, &mut cx);
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert!(
        log.borrow()
            .contains(&crate::module::recording::Recorded::Command(
                tile,
                "unpin".into()
            )),
        "{:?}",
        log.borrow()
    );
    let focused = shell.read_with(&cx, |s, _| s.focus_handle.clone());
    assert!(
        cx.update(|window, _| focused.is_focused(window)),
        "focus back on the shell"
    );
}

#[gpui::test]
fn completions_rank_accept_on_tab_and_submit_on_a_unique_enter(cx: &mut gpui::TestAppContext) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes(":");
    cx.simulate_input("sort g");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("completion-row-0").is_some(),
        "gamma01 is offered"
    );
    assert!(
        cx.debug_bounds("completion-row-1").is_none(),
        "delta01 has no g"
    );
    cx.simulate_keystrokes("tab");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let shell = shell_of(&window, &mut cx);
    let line = shell.read_with(&cx, |s, cx| s.command_input.read(cx).value().to_string());
    assert_eq!(line, "sort gamma01");

    cx.simulate_keystrokes("enter");
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert!(
        log.borrow()
            .contains(&crate::module::recording::Recorded::Command(
                tile,
                "sort gamma01".into()
            ))
    );

    // A unique match submits without tab.
    cx.simulate_keystrokes(":");
    cx.simulate_input("sort del");
    cx.simulate_keystrokes("enter");
    assert!(
        log.borrow()
            .contains(&crate::module::recording::Recorded::Command(
                tile,
                "sort delta01".into()
            )),
        "{:?}",
        log.borrow()
    );
}

/// C1, final review: a second `tab` used to corrupt the line, because
/// `CommandLine::word` was only ever refreshed by
/// `on_command_line_changed` (which `InputEvent::Change` drives), and
/// the Accept branch's `set_value` emits no `Change` at the pinned
/// gpui-component rev — so the *second* accept spliced the new
/// candidate into the byte range the *first* accept had already made
/// stale. Both `delta01` and `gamma01` match "a01" (the branch's own
/// fixture — see `RecordingFactory::new`), so this exercises the
/// two-candidate cycle the single-tab tests never reach a second time.
#[gpui::test]
fn a_second_tab_cycles_the_completion_instead_of_corrupting_the_line(
    cx: &mut gpui::TestAppContext,
) {
    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes(":");
    cx.simulate_input("sort a01");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let shell = shell_of(&window, &mut cx);

    cx.simulate_keystrokes("tab");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let line = shell.read_with(&cx, |s, cx| s.command_input.read(cx).value().to_string());
    assert_eq!(line, "sort delta01", "first tab accepts the top candidate");

    cx.simulate_keystrokes("tab");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let line = shell.read_with(&cx, |s, cx| s.command_input.read(cx).value().to_string());
    assert_eq!(
        line, "sort gamma01",
        "second tab cycles cleanly to the next candidate — a stale \
         `c.word` would instead splice into the wrong range and \
         produce \"sort gamma01ta01\""
    );
}

#[gpui::test]
fn an_ambiguous_enter_and_a_failing_command_show_inline_and_stay_open(
    cx: &mut gpui::TestAppContext,
) {
    let (mut services, log) = services_with_recorder();
    // Make the recorder's `command` fail.
    let mut roster = crate::module::ModuleRoster::new("rec");
    let mut rec = crate::module::recording::RecordingFactory::new("rec");
    rec.command_result = Err("no such column".into());
    let log2 = rec.log.clone();
    roster.add(Box::new(rec));
    services.roster = roster;
    let _ = log;
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes(":");
    cx.simulate_input("sort a01");
    cx.simulate_keystrokes("enter");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let shell = shell_of(&window, &mut cx);
    let error = shell.read_with(&cx, |s, _| {
        s.command_line.as_ref().and_then(|c| c.error.clone())
    });
    assert!(
        error
            .as_deref()
            .is_some_and(|e| e.contains("delta01") && e.contains("gamma01")),
        "{error:?}"
    );
    assert!(
        log2.borrow()
            .iter()
            .all(|r| !matches!(r, crate::module::recording::Recorded::Command(..))),
        "nothing ran"
    );

    cx.simulate_keystrokes("tab");
    cx.simulate_keystrokes("enter");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let error = shell.read_with(&cx, |s, _| {
        s.command_line.as_ref().and_then(|c| c.error.clone())
    });
    assert_eq!(
        error.as_deref(),
        Some("no such column"),
        "the occupant's error, inline, line still open"
    );
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("command-line").is_none());
}

#[gpui::test]
fn slash_streams_find_events_and_escape_cancels(cx: &mut gpui::TestAppContext) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("/");
    cx.simulate_input("sp");
    let shell = shell_of(&window, &mut cx);
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    use crate::module::{FindEvent, recording::Recorded};
    assert!(
        log.borrow()
            .contains(&Recorded::Find(tile, FindEvent::Changed("sp".into()))),
        "{:?}",
        log.borrow()
    );
    cx.simulate_keystrokes("escape");
    assert!(
        log.borrow()
            .contains(&Recorded::Find(tile, FindEvent::Cancelled))
    );
    cx.simulate_keystrokes("/");
    cx.simulate_input("x");
    cx.simulate_keystrokes("enter");
    assert!(
        log.borrow()
            .contains(&Recorded::Find(tile, FindEvent::Committed("x".into())))
    );
}

/// Fix round 1, finding 1: `ctrl+k` is a shipped, always-reachable
/// binding, so it must still open the palette (and clean up after
/// itself) even from inside an open `:` line, rather than the line
/// swallowing it silently and staying stuck open.
#[gpui::test]
fn ctrl_k_cancels_an_open_command_line_and_opens_the_palette(cx: &mut gpui::TestAppContext) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes(":");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("command-line").is_some(),
        "line open before ctrl-k"
    );
    cx.simulate_keystrokes("ctrl-k");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("command-line").is_none(),
        "ctrl-k should have cancelled the open command line"
    );
    let shell = shell_of(&window, &mut cx);
    assert!(
        shell.read_with(&cx, |s, _| s.palette.is_some()),
        "ctrl-k should still open the palette"
    );
    // A `Command` prompt cancel is silent: nothing was submitted, and
    // (unlike `/`) nothing is cancelled on the occupant either.
    assert!(
        log.borrow().iter().all(|r| !matches!(
            r,
            crate::module::recording::Recorded::Command(..)
                | crate::module::recording::Recorded::Find(..)
        )),
        "{:?}",
        log.borrow()
    );
}

/// Fix round 1, finding 1: opening a shell dialog over an open `/`
/// line must cancel it (mirroring the existing `close_palette` call
/// in `dialog::open_shell_dialog_with_key`) — otherwise the line
/// stays `Some`, still painted, but the modal branch in
/// `handle_key_down` is checked first and would swallow every key
/// meant for it from then on. Dispatches `settings::open` directly,
/// the same real path `settings_open_opens_the_modal` above uses,
/// rather than a raw `ctrl-,` keystroke: this is testing what opening
/// a dialog does to an open command line, not how the dialog itself
/// gets reached.
#[gpui::test]
fn opening_a_shell_dialog_cancels_an_open_command_line(cx: &mut gpui::TestAppContext) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("/");
    cx.simulate_input("sp");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("command-line").is_some(),
        "line open before the dialog"
    );
    let shell = shell_of(&window, &mut cx);
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("settings::open".to_string()), None, window, cx);
        });
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("command-line").is_none(),
        "opening the dialog should have cancelled the open command line"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.modal.is_some()),
        "the dialog should still have opened"
    );
    use crate::module::{FindEvent, recording::Recorded};
    assert_eq!(
        log.borrow().last(),
        Some(&Recorded::Find(tile, FindEvent::Cancelled)),
        "{:?}",
        log.borrow()
    );
}

/// Fix round 1, finding 2: a mouse-down on a different tile is the one
/// way the workspace's focused tile can change while a command line
/// is open (every keystroke is claimed ahead of the matcher), so it
/// must cancel the line rather than leaving it painted under the
/// wrong tile — this is what keeps "focused tile == command_line.tile
/// while open" an invariant (see the comment where the strip is
/// painted). Same click-point layout math as `mouse_down_on_a_tile_
/// focuses_it` below.
#[gpui::test]
fn a_mouse_down_on_another_tile_cancels_an_open_command_line(cx: &mut gpui::TestAppContext) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    // Two tiles, so there is a second, non-focused one to click.
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    let shell = shell_of(&window, &mut cx);
    let opened_on = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    cx.simulate_keystrokes(":");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("command-line").is_some(),
        "line open before the click"
    );

    let (target_id, click_point) = cx.update(|window, cx| {
        let viewport = window.viewport_size();
        let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
        let tile_width = (f32::from(viewport.width) - sidebar::WIDTH).max(0.0);
        let content_height =
            (f32::from(viewport.height) - toolbar_height - status::HEIGHT).max(0.0);
        let rects = shell
            .read(cx)
            .services
            .workspaces
            .active()
            .tree()
            .layout(Rect {
                x: 0.0,
                y: 0.0,
                w: tile_width,
                h: content_height,
            });
        let (id, r) = rects
            .into_iter()
            .find(|(id, _)| Some(*id) != Some(opened_on))
            .expect("a second, non-focused tile exists");
        let point = gpui::point(
            px(sidebar::WIDTH + r.x + r.w / 2.0),
            px(toolbar_height + r.y + r.h / 2.0),
        );
        (id, point)
    });

    cx.simulate_mouse_down(click_point, MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_up(click_point, MouseButton::Left, gpui::Modifiers::none());
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert!(
        cx.debug_bounds("command-line").is_none(),
        "the click should have cancelled the open command line"
    );
    let focused = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_eq!(focused, target_id, "the click still moved focus");
    let shell_focus = shell.read_with(&cx, |s, _| s.focus_handle.clone());
    assert!(
        cx.update(|window, _| shell_focus.is_focused(window)),
        "focus should have returned to the shell root, then re-armed \
         by the click's own restore"
    );
    assert!(
        log.borrow().iter().all(|r| !matches!(
            r,
            crate::module::recording::Recorded::Command(..)
                | crate::module::recording::Recorded::Find(..)
        )),
        "a Command prompt's cancel is silent: {:?}",
        log.borrow()
    );
}

/// I1, final review: the sidebar's workspace-switch mouse-down
/// (`sidebar::sidebar`) dispatches `workspace::switch_N` directly —
/// there is no sidebar-click precedent to imitate instead, so this
/// dispatches the same action the real mouse-down does, the way
/// `switching_away_and_back_within_one_frame_voids_the_drop` already
/// does for the identical reason. Switching workspaces changes which
/// tile the active workspace considers focused without ever touching
/// the command line or moving keyboard focus, so nothing in the
/// pre-fix code cancelled the line: the strip kept painting over the
/// OLD workspace's tile while `enter` would have run the line against
/// a tile the switch just left. The render-time backstop (see the
/// comment beside `ensure_occupants`'s drag-cancel neighbours in
/// `render`) is what closes this.
#[gpui::test]
fn switching_workspaces_cancels_an_open_command_line(cx: &mut gpui::TestAppContext) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes(":");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("command-line").is_some(),
        "line open before the switch"
    );

    let shell = shell_of(&window, &mut cx);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(
                &ActionId("workspace::switch_2".to_string()),
                None,
                window,
                cx,
            );
        });
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    // The direct state check, not just the painted strip: workspace 2
    // has no tile of its own, so `focused_rect` would be `None` there
    // regardless of whether the line was actually cancelled — the
    // strip's absence alone cannot isolate this mutation from "there
    // is nowhere to paint it this frame".
    assert!(
        shell.read_with(&cx, |s, _| s.command_line.is_none()),
        "switching workspaces should have cancelled the open command line"
    );
    assert!(
        cx.debug_bounds("command-line").is_none(),
        "and the strip should not be painted either"
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active_index()),
        2,
        "the switch itself still happened"
    );
    assert!(
        log.borrow().iter().all(|r| !matches!(
            r,
            crate::module::recording::Recorded::Command(..)
                | crate::module::recording::Recorded::Find(..)
        )),
        "a Command prompt's cancel is silent: {:?}",
        log.borrow()
    );
}

/// I1, final review, the other half: a click into the toolbar's
/// `filter_input` steals keyboard focus from `command_input` without
/// touching the workspace at all — the opposite failure shape from
/// `switching_workspaces_cancels_an_open_command_line`'s above, and
/// the other arm of the same render-time backstop's `||`. Focuses
/// `filter_input`'s handle directly, the same real path
/// `escape_in_the_filter_input_returns_focus_to_the_shell_root`
/// already uses instead of a pixel-coordinate click.
#[gpui::test]
fn clicking_the_filter_input_cancels_an_open_command_line(cx: &mut gpui::TestAppContext) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes(":");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("command-line").is_some(),
        "line open before the filter is focused"
    );

    let shell = shell_of(&window, &mut cx);
    let filter_input = shell.read_with(&cx, |s, _| s.filter_input.clone());
    let filter_focus = filter_input.read_with(&cx, |state, cx| state.focus_handle(cx));
    cx.update(|window, cx| filter_focus.focus(window, cx));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert!(
        cx.debug_bounds("command-line").is_none(),
        "focusing the filter input should have cancelled the open command line"
    );
    assert!(
        cx.update(|window, _| filter_focus.is_focused(window)),
        "the filter still took focus"
    );
    assert!(
        log.borrow().iter().all(|r| !matches!(
            r,
            crate::module::recording::Recorded::Command(..)
                | crate::module::recording::Recorded::Find(..)
        )),
        "a Command prompt's cancel is silent: {:?}",
        log.borrow()
    );
}
