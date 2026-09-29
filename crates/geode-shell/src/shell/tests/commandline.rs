//! The `/` and `:` command-line seam: opening, completion, accept,
//! and the cancel triggers that close it from elsewhere in the shell.

use super::*;

#[gpui::test]
fn fzf_tab_folds_a_branch_without_changing_input_or_committing(cx: &mut gpui::TestAppContext) {
    use crate::fuzzyfind::{FindItem, FindTree};
    let (services, _) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    shell.update(&mut cx, |s, _| s.find_style = FindStyle::Fzf);
    cx.simulate_keystrokes("ctrl-v /");
    let results = shell.read_with(&cx, |s, _| s.fuzzy_find.clone().unwrap());
    results.update(&mut cx, |r, cx| {
        r.replace_tree_items(
            vec![
                FindItem::new("parent", "Parent", "", |_, _, _| Ok(())),
                FindItem::new("child", "Child", "Parent", |_, _, _| Ok(())),
            ],
            std::sync::Arc::new(FindTree::from_depths([0, 1])),
            cx,
        )
    });
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    cx.simulate_keystrokes("down");
    assert_eq!(
        results.read_with(&cx, |r, _| r.selected_item().unwrap().label().to_string()),
        "Parent"
    );
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    cx.simulate_keystrokes("down");
    assert_eq!(
        results.read_with(&cx, |r, _| r.selected_item().unwrap().label().to_string()),
        "Child"
    );
    assert_eq!(results.read_with(&cx, |r, _| r.query().to_string()), "");
    assert!(shell.read_with(&cx, |s, _| s.command_line.is_some()));
    cx.simulate_keystrokes("escape");
    assert!(!results.read_with(&cx, |r, _| r.is_active()));
}

#[gpui::test]
fn fzf_ranks_picks_and_cancels_without_moving_the_tree(cx: &mut gpui::TestAppContext) {
    use crate::module::recording::{Recorded, RecordingFactory};
    let (mut services, _) = services_with_recorder();
    let mut recorder = RecordingFactory::new("rec");
    recorder.completions = ["Set panel list toggle", "Split right", "split", "splitter"]
        .map(str::to_string)
        .to_vec();
    let log = recorder.log.clone();
    let mut roster = crate::module::ModuleRoster::new();
    roster.add(Box::new(recorder));
    services.roster = roster;
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    shell.update(&mut cx, |s, _| s.find_style = FindStyle::Fzf);
    cx.simulate_keystrokes("ctrl-v /");
    cx.simulate_input("split");
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("fuzzy-find").is_some());
    assert_eq!(
        shell.read_with(&cx, |s, cx| s
            .fuzzy_find
            .as_ref()
            .unwrap()
            .read(cx)
            .selected_item()
            .unwrap()
            .label()
            .to_string()),
        "split"
    );
    assert!(
        !log.borrow()
            .iter()
            .any(|r| matches!(r, Recorded::Find(..) | Recorded::FindPick(..))),
        "typing does not touch the tree"
    );
    cx.simulate_keystrokes("down enter");
    assert!(
        log.borrow()
            .iter()
            .any(|r| matches!(r, Recorded::FindPick(_, label) if label == "Split right"))
    );
    assert!(shell.read_with(&cx, |s, _| s.command_line.is_none()));

    log.borrow_mut().clear();
    cx.simulate_keystrokes("/");
    cx.simulate_input("spltr");
    cx.run_until_parked();
    assert!(
        shell.read_with(&cx, |s, cx| s
            .fuzzy_find
            .as_ref()
            .unwrap()
            .read(cx)
            .selected_item()
            .is_some()),
        "subsequence matching"
    );
    cx.simulate_keystrokes("escape");
    assert!(log.borrow().is_empty(), "escape leaves the tile untouched");
    assert!(shell.read_with(&cx, |s, _| s.fuzzy_find.is_none()));

    cx.simulate_keystrokes("/");
    cx.simulate_input("zzzz");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    assert!(
        shell.read_with(&cx, |s, _| s.command_line.is_some()),
        "no matches stays open"
    );
    assert!(log.borrow().is_empty());
    cx.simulate_keystrokes("escape");

    // The setting takes effect on the next prompt; Vim still receives incremental events.
    shell.update(&mut cx, |s, _| s.find_style = FindStyle::Vim);
    cx.simulate_keystrokes("/");
    cx.simulate_input("split");
    cx.run_until_parked();
    assert!(shell.read_with(&cx, |s, _| s.fuzzy_find.is_none()));
    assert!(log.borrow().iter().any(
        |r| matches!(r, Recorded::Find(_, crate::module::FindEvent::Changed(q)) if q == "split")
    ));
}

#[gpui::test]
fn fzf_scrolls_to_keyboard_selection_and_accepts_pointer_picks(cx: &mut gpui::TestAppContext) {
    use crate::module::recording::{Recorded, RecordingFactory};
    let (mut services, _) = services_with_recorder();
    let mut recorder = RecordingFactory::new("rec");
    recorder.completions = (0..80).map(|i| format!("Row {i:02}")).collect();
    let log = recorder.log.clone();
    let mut roster = crate::module::ModuleRoster::new();
    roster.add(Box::new(recorder));
    services.roster = roster;
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    shell.update(&mut cx, |s, _| s.find_style = FindStyle::Fzf);
    cx.simulate_keystrokes("ctrl-v / ctrl-f ctrl-f ctrl-f ctrl-f");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let row = cx
        .debug_bounds("find-result-40")
        .expect("selected row is rendered");
    let viewport = cx.debug_bounds("find-results").unwrap();
    assert!(row.top() >= viewport.top() && row.bottom() <= viewport.bottom());
    cx.simulate_mouse_down(row.center(), MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_up(row.center(), MouseButton::Left, gpui::Modifiers::none());
    assert!(
        log.borrow()
            .iter()
            .any(|r| matches!(r, Recorded::FindPick(_, label) if label == "Row 40"))
    );
    assert!(shell.read_with(&cx, |s, _| s.command_line.is_none()));
}

#[gpui::test]
fn completion_navigation_keeps_the_highlight_visible(cx: &mut gpui::TestAppContext) {
    let (mut services, _) = services_with_recorder();
    let mut recorder = crate::module::recording::RecordingFactory::new("rec");
    recorder.completions = (0..12).map(|i| format!("command{i:02}")).collect();
    let mut roster = crate::module::ModuleRoster::new();
    roster.add(Box::new(recorder));
    services.roster = roster;
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v :");
    let shell = shell_of(&window, &mut cx);

    for (keys, selected, selector) in [
        (
            "down down down down down down down down",
            8,
            "completion-row-8",
        ),
        ("ctrl-n ctrl-n ctrl-n", 11, "completion-row-11"),
        ("down", 0, "completion-row-0"),
        ("ctrl-p", 11, "completion-row-11"),
        ("up", 10, "completion-row-10"),
        ("tab", 11, "completion-row-11"),
    ] {
        cx.simulate_keystrokes(keys);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(
            shell.read_with(&cx, |s, _| s.command_line.as_ref().unwrap().highlighted),
            selected
        );
        let row = cx
            .debug_bounds(selector)
            .expect("selected completion is rendered");
        let viewport = cx
            .debug_bounds("completion-list")
            .expect("completion viewport");
        assert!(
            row.top() >= viewport.top() && row.bottom() <= viewport.bottom(),
            "selected row {row:?} must be fully visible in {viewport:?}"
        );
        assert!(
            viewport.size.height < row.size.height * 12.0,
            "the popup stays capped instead of expanding to fit all candidates"
        );
    }

    // Editing re-ranks from the first result; reopening starts at the top too.
    for keys in ["backspace", "escape :"] {
        cx.simulate_keystrokes(keys);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(
            shell.read_with(&cx, |s, _| s.command_line.as_ref().unwrap().highlighted),
            0
        );
        let row = cx.debug_bounds("completion-row-0").unwrap();
        let viewport = cx.debug_bounds("completion-list").unwrap();
        assert!(row.top() >= viewport.top() && row.bottom() <= viewport.bottom());
    }
}

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
    assert!(
        cx.debug_bounds("completion-row-0").is_some()
            && cx.debug_bounds("completion-row-1").is_some(),
        "the occupant's suggestions appear immediately, before typing"
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

/// Repeated completion must refresh the active word range after each acceptance.
/// Programmatic `set_value` emits no change event, so relying on that event would
/// splice the second candidate into stale offsets. Two matching fixture candidates
/// exercise the full cycle.
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
    let mut roster = crate::module::ModuleRoster::new();
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

/// `ctrl+k` opens the palette from an open command line and closes the line, preserving
/// the chord's reachability and overlay cleanup.
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

/// Opening a shell dialog cancels an open find line. Otherwise the modal would claim
/// the keys while leaving an unusable line painted beneath it. Dispatch
/// `settings::open` directly to isolate overlay cleanup from the settings shortcut.
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
        shell.read_with(&cx, |s, _| s.modal_open()),
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

/// A mouse-down on another tile cancels an open command line, keeping the line's owner
/// equal to the focused tile. Keyboard events are claimed by the line, so the mouse is
/// the path that can move tile focus while it is open.
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
        let tile_width = (f32::from(viewport.width) - sidebar::width(window)).max(0.0);
        let content_height =
            (f32::from(viewport.height) - toolbar_height - status::height(window)).max(0.0);
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
            px(sidebar::width(window) + r.x + r.w / 2.0),
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

/// A tile mouse-down commits a nonempty find line, preserving its match and repeat
/// target; an empty find line cancels. A command line cancels regardless of its text.
#[gpui::test]
fn a_mouse_down_on_a_tile_commits_an_open_find_line(cx: &mut gpui::TestAppContext) {
    use crate::module::{FindEvent, recording::Recorded};
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    let shell = shell_of(&window, &mut cx);
    let opened_on = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    cx.simulate_keystrokes("/");
    cx.simulate_input("sp");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let (_target_id, click_point) = cx.update(|window, cx| {
        let viewport = window.viewport_size();
        let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
        let tile_width = (f32::from(viewport.width) - sidebar::width(window)).max(0.0);
        let content_height =
            (f32::from(viewport.height) - toolbar_height - status::height(window)).max(0.0);
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
            px(sidebar::width(window) + r.x + r.w / 2.0),
            px(toolbar_height + r.y + r.h / 2.0),
        );
        (id, point)
    });

    cx.simulate_mouse_down(click_point, MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_up(click_point, MouseButton::Left, gpui::Modifiers::none());
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert!(cx.debug_bounds("command-line").is_none(), "the line closed");
    assert!(
        log.borrow().contains(&Recorded::Find(
            opened_on,
            FindEvent::Committed("sp".into())
        )),
        "the click committed the find: {:?}",
        log.borrow()
    );
    assert!(
        !log.borrow()
            .contains(&Recorded::Find(opened_on, FindEvent::Cancelled)),
        "and did not cancel it"
    );

    // An empty `/` line is a cancel (vimfind's own rule).
    cx.simulate_keystrokes("/");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let now_on = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    cx.simulate_mouse_down(click_point, MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_up(click_point, MouseButton::Left, gpui::Modifiers::none());
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        log.borrow()
            .contains(&Recorded::Find(now_on, FindEvent::Cancelled)),
        "an empty find line cancels on click-away: {:?}",
        log.borrow()
    );
}

/// Switching workspaces cancels an open command line so it cannot remain attached to
/// the previous workspace's tile. Dispatch the same switch action as the sidebar to
/// exercise the render-time ownership check.
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

/// Moving keyboard focus from the command input to the toolbar filter cancels the line
/// even when tile focus is unchanged. Set the filter's focus handle directly to isolate
/// this arm of the render-time ownership check.
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
