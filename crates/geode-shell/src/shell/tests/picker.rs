//! The dimension picker (Phase 4a §3.3, §3.4): the real key-dispatch and
//! `Request::Distinct` pipeline, end to end — `shell::picker`'s own
//! `mod tests` covers the pure core, this covers the wiring: opening
//! through `frame::pick_<column>`, the emitted `ShellEvent::
//! DistinctRequested`, `ShellView::deliver_distinct`'s stale-tag guard,
//! and the keyboard vocabulary (`tab`/`ctrl+a`/`ctrl+x`/`enter`/`escape`)
//! driven through the same `GeodeModal` reclaims (`dialog::
//! init_reclaimed_keybindings`) production uses.

use super::*;
use geode_core::query::DistinctOutcome;
use geode_core::scope::{DimensionSelection, Scope};

/// A trimmed dataset doc, just enough to make `book` pickable
/// (categorical dimension) and to let `lhu`'s name appear in a scope
/// without needing its own schema entry (a `Scope` is a plain value —
/// nothing here validates it against the schema before `Frame::
/// set_scope` accepts it). Same shape as `shell::picker::tests::
/// CARRIED_DEMO`, trimmed further since this file only needs `book`
/// pickable, not the exact `pickable_columns` ordering that fixture
/// exists to pin.
const DATASETS_DOC: &str = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
"#;

/// `test_services()` with a real `datasets` doc (so `book` is pickable)
/// and `register_pick_actions` run over it, so `frame::pick_book` is a
/// real, dispatchable action id — the two-step registration `main.rs`
/// itself does (`register_builtin_actions` then `register_pick_actions`),
/// reproduced here rather than through `test_services()` (whose whole
/// point is an *empty* config — see that function's own comment on why
/// it still calls `register_pick_actions` anyway, over nothing).
fn services_with_pickable() -> ShellServices {
    let config = Config::load(&ConfigSources {
        builtin: vec![LayerDoc::builtin("datasets", DATASETS_DOC).unwrap()],
        desk: None,
        user: None,
    });
    let mut registry = ActionRegistry::default();
    register_builtin_actions(&mut registry);
    register_pick_actions(&mut registry, &pickable_columns(&config));
    let mod_alias = default_mod();
    let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
    let (keymap, diags) = build_keymap(&[doc], mod_alias, &registry);
    assert!(diags.is_empty(), "{diags:?}");
    let (theme, warnings) = crate::theme::load_bundled();
    assert!(warnings.is_empty(), "{warnings:?}");
    ShellServices {
        config,
        registry,
        keymap,
        mod_alias,
        workspaces: Workspaces::new(),
        theme,
        session_path: None,
        roster: crate::module::ModuleRoster::default(),
        restored_tiles: crate::session::TileRecords::new(),
        restored_frame: None,
    }
}

/// Dispatch `action` through `ShellView::dispatch` directly — the same
/// route `shell::tests::dialog_test_shell` uses to open the keybinding/
/// settings dialogs, reused here rather than driving the palette (`frame::
/// pick_book` has no keymap binding of its own — it's palette-only,
/// "Pick: book" — so a keystroke-only path would have to go through the
/// palette's own filter-and-enter dance for no benefit over calling the
/// one method every dispatch route already funnels through).
fn dispatch_action(shell: &Entity<ShellView>, action: &str, vcx: &mut gpui::VisualTestContext) {
    vcx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId(action.to_string()), None, window, cx);
        });
    });
}

#[gpui::test]
fn the_picker_requests_values_minus_its_own_selection_and_applies_ticks_as_one_scope_change(
    cx: &mut gpui::TestAppContext,
) {
    let (window, mut vcx) = open_shell(cx, services_with_pickable());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());

    let requested = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    vcx.update(|_, cx| {
        let requested = requested.clone();
        cx.subscribe(&shell, move |_, e: &ShellEvent, _| {
            if let ShellEvent::DistinctRequested(p) = e {
                requested.borrow_mut().push(p.clone());
            }
        })
        .detach();
    });

    frame.update(&mut vcx, |f, cx| {
        f.set_scope(Scope {
            dimensions: vec![
                DimensionSelection {
                    column: "book".into(),
                    values: vec!["BK000".into()],
                },
                DimensionSelection {
                    column: "lhu".into(),
                    values: vec!["L1".into()],
                },
            ],
            ..Scope::default()
        });
        cx.notify();
    });

    dispatch_action(&shell, "frame::pick_book", &mut vcx);

    let req = requested
        .borrow()
        .last()
        .cloned()
        .expect("a distinct request");
    assert_eq!(req.column, "book");
    assert_eq!(req.scope.dimensions.len(), 1, "own selection removed");
    assert_eq!(req.scope.dimensions[0].column, "lhu");

    // Deliver values; BK000 is pre-ticked (the scope's own selection
    // before the picker ever asked).
    shell.update(&mut vcx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: PICKER_KEY,
                tag: req.tag,
                column: "book".into(),
                values: Ok(vec![
                    ("BK000".into(), 5),
                    ("BK001".into(), 7),
                    ("BK002".into(), 1),
                ]),
            },
            cx,
        )
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("picker-value-BK001").is_some());

    // `ctrl+a`/`ctrl+x` (the `GeodeModal` reclaim `dialog::
    // init_reclaimed_keybindings` registers) through the real key
    // pipeline: tick every shown value, then clear all — proving both
    // keys actually reach the picker's handler rather than being eaten
    // by gpui-component's own `Input` bindings on this key (`ctrl+a` is
    // `MoveHome` on macOS, `SelectAll` elsewhere — either way, a real
    // collision this environment can exercise).
    vcx.simulate_keystrokes("ctrl-a");
    let ticked = shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().ticked.clone());
    assert_eq!(
        ticked.len(),
        3,
        "ctrl+a must tick every value the filter currently shows"
    );
    vcx.simulate_keystrokes("ctrl-x");
    let ticked = shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().ticked.clone());
    assert!(ticked.is_empty(), "ctrl+x must clear every tick");
    // `ctrl+x` just wiped BK000's pre-tick along with everything else —
    // restore it (the selection is still at index 0, BK000, since
    // neither `ctrl+a` nor `ctrl+x` touch `selected`) before continuing
    // into the scripted flow below, which depends on BK000 starting
    // ticked.
    vcx.simulate_keystrokes("tab");

    let v0 = frame.read_with(&vcx, |f, _| f.versions().scope);
    vcx.simulate_keystrokes("down tab enter"); // tick BK001 (BK000 re-ticked above), apply
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.versions().scope),
        v0 + 1,
        "one scope change"
    );
    let books = frame.read_with(&vcx, |f, _| {
        f.scope()
            .dimensions
            .iter()
            .find(|d| d.column == "book")
            .unwrap()
            .values
            .clone()
    });
    assert_eq!(books, vec!["BK000".to_string(), "BK001".to_string()]);
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
}

#[gpui::test]
fn a_stale_distinct_outcome_is_dropped(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_pickable());
    let shell = shell_of(&window, &mut vcx);

    dispatch_action(&shell, "frame::pick_book", &mut vcx);
    let tag = shell.read_with(&vcx, |s, _| s.picker.as_ref().expect("picker open").tag);
    assert_eq!(
        tag, 1,
        "the one request_values call `open` makes bumped the tag to 1"
    );

    // A tag older than the picker's latest request (0 < 1): dropped —
    // still loading.
    shell.update(&mut vcx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: PICKER_KEY,
                tag: 0,
                column: "book".into(),
                values: Ok(vec![("BK000".into(), 1)]),
            },
            cx,
        )
    });
    assert!(
        shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().values.is_none()),
        "a stale-tag outcome must be dropped, leaving the picker still \"loading…\""
    );

    // A different column entirely, even with the right tag: also
    // dropped — the picker moved on (or never asked about this column).
    shell.update(&mut vcx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: PICKER_KEY,
                tag: 1,
                column: "lhu".into(),
                values: Ok(vec![("L1".into(), 1)]),
            },
            cx,
        )
    });
    assert!(
        shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().values.is_none()),
        "an outcome for a different column must be dropped too"
    );

    // The real (matching column and tag) outcome still applies.
    shell.update(&mut vcx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: PICKER_KEY,
                tag: 1,
                column: "book".into(),
                values: Ok(vec![("BK000".into(), 1)]),
            },
            cx,
        )
    });
    assert!(
        shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().values.is_some()),
        "the matching outcome must still be delivered"
    );
}

#[gpui::test]
fn escape_cancels_without_touching_the_scope(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_pickable());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());

    frame.update(&mut vcx, |f, cx| {
        f.set_scope(Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into()],
            }],
            ..Scope::default()
        });
        cx.notify();
    });
    let v0 = frame.read_with(&vcx, |f, _| f.versions().scope);

    dispatch_action(&shell, "frame::pick_book", &mut vcx);
    shell.update(&mut vcx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: PICKER_KEY,
                tag: 1,
                column: "book".into(),
                values: Ok(vec![("BK000".into(), 1), ("BK001".into(), 2)]),
            },
            cx,
        )
    });
    vcx.run_until_parked();

    // Tick a second value but never apply — escape must throw this away.
    vcx.simulate_keystrokes("down tab");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().ticked.len()),
        2,
        "BK000 (pre-ticked) plus BK001 (just ticked)"
    );

    vcx.simulate_keystrokes("escape");

    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
    assert!(
        shell.read_with(&vcx, |s, _| s.picker.is_none()),
        "close_modal clears the picker like every other dialog"
    );
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.versions().scope),
        v0,
        "escape must not touch the scope"
    );
    let books = frame.read_with(&vcx, |f, _| {
        f.scope()
            .dimensions
            .iter()
            .find(|d| d.column == "book")
            .unwrap()
            .values
            .clone()
    });
    assert_eq!(
        books,
        vec!["BK000".to_string()],
        "the original scope is untouched"
    );
}
