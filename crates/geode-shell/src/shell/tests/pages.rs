//! The page seam from the shell's side: toggle, close, context stack,
//! visibility announcements, and the insert-mode route for page inputs.

use super::{
    dispatch_action, open_shell, services_with_page, services_with_recorders, shell_of, with_pages,
};
use crate::module::recording::{PageRecorded, RecordingPageFactory};
use crate::shell::ShellView;

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
    assert!(cx.update(|window, cx| {
        let s = shell.read(cx);
        s.page
            .as_ref()
            .unwrap()
            .occupant
            .content
            .focus_handle(cx)
            .is_focused(window)
    }));

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
        assert!(s.notice.is_some(), "refused with a notice");
    });
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

#[gpui::test]
fn tile_bindings_are_inert_while_a_page_is_open(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(
        cx,
        services_with_page(RecordingPageFactory::new("diagnostics")),
    );
    let shell = shell_of(&window, &mut cx);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    cx.simulate_keystrokes("alt-n");
    shell.read_with(&cx, |s, _| {
        assert!(
            !s.modal_open(),
            "tile::add's picker did not open: refused over the page"
        );
        assert!(s.page_open());
        assert!(s.notice.is_some(), "refused with a notice");
    });
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
