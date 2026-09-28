//! Dimension-picker integration through real key dispatch and distinct-value requests:
//! opening by action, request events, stale delivery rejection, and modal keyboard
//! handling. Pure picker state is tested in `shell::picker`.

use super::*;
use crate::shell::picker;
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
pub(super) fn services_with_pickable() -> ShellServices {
    let (config, builtin) = ShellServices::config_and_builtin(ConfigSources {
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
        builtin,
        registry,
        keymap,
        mod_alias,
        workspaces: Workspaces::new(),
        theme,
        session_path: None,
        roster: crate::module::ModuleRoster::default(),
        restored_tiles: crate::session::TileRecords::new(),
        restored_frame: None,
        restored_pinned: Default::default(),
        restored_palette_usage: crate::palette_usage::PaletteUsage::new(),
        log: None,
        action_tail: std::sync::Arc::new(std::sync::Mutex::new(
            crate::diagnostics::ActionTail::new(),
        )),
        keymap_diagnostics: Vec::new(),
        keymap_fragments: Vec::new(),
        keymap_fragment_diagnostics: Vec::new(),
    }
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
        f.shared_mut().set_scope(Scope {
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

    let v0 = frame.read_with(&vcx, |f, _| f.shared().versions().scope);
    vcx.simulate_keystrokes("down tab enter"); // tick BK001 (BK000 re-ticked above), apply
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().versions().scope),
        v0 + 1,
        "one scope change"
    );
    let books = frame.read_with(&vcx, |f, _| {
        f.shared()
            .scope()
            .dimensions
            .iter()
            .find(|d| d.column == "book")
            .unwrap()
            .values
            .clone()
    });
    assert_eq!(books, vec!["BK000".to_string(), "BK001".to_string()]);
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
}

/// A frame scope naming an undefined expression is never sent as a
/// distinct request: the picker's values area shows the resolution error,
/// delivered on the picker's own key and latest tag.
#[gpui::test]
fn the_picker_shows_an_unresolved_named_expression_instead_of_requesting(
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
        f.shared_mut().set_scope(Scope {
            named: vec!["gone".into()],
            ..Scope::default()
        });
        cx.notify();
    });

    dispatch_action(&shell, "frame::pick_book", &mut vcx);
    vcx.run_until_parked();

    assert!(
        requested.borrow().is_empty(),
        "no request for an unresolved scope: {:?}",
        requested.borrow()
    );
    let values = shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().values.clone());
    assert_eq!(
        values,
        Some(Err("named expression 'gone' is missing".to_string()))
    );
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

/// Clicking a scope chip's body opens its column's Values stage and emits a
/// distinct-value request, matching the corresponding picker action. Click the rendered
/// chip to exercise the mouse handler.
#[gpui::test]
fn clicking_a_scope_chips_body_opens_the_picker_on_that_column(cx: &mut gpui::TestAppContext) {
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
        f.shared_mut().set_scope(Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into()],
            }],
            ..Scope::default()
        });
        cx.notify();
    });
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.run_until_parked();

    let chip_bounds = vcx
        .debug_bounds("scope-chip-book")
        .expect("the book chip's body should have painted");
    let center = gpui::point(
        chip_bounds.origin.x + chip_bounds.size.width / 2.0,
        chip_bounds.origin.y + chip_bounds.size.height / 2.0,
    );
    vcx.simulate_mouse_down(center, MouseButton::Left, gpui::Modifiers::none());
    vcx.run_until_parked();

    assert!(
        shell.read_with(&vcx, |s, _| s.modal_open()),
        "clicking the chip body should have opened the picker modal"
    );
    let stage = shell.read_with(&vcx, |s, _| s.picker.as_ref().map(|p| p.stage.clone()));
    assert_eq!(
        stage,
        Some(crate::shell::picker::Stage::Values {
            column: "book".to_string()
        }),
        "the picker should have opened straight onto the clicked chip's column"
    );

    let req = requested
        .borrow()
        .last()
        .cloned()
        .expect("a distinct request for book");
    assert_eq!(req.column, "book");
}

/// A scope-chip tooltip shows the unelided selection (`Chip::full`) and the picker
/// chord. The fixture matches the click test, but hovering need not dispatch the picker
/// action.
#[gpui::test]
fn hovering_a_scope_chip_shows_the_full_selection_and_the_picker_chord(
    cx: &mut gpui::TestAppContext,
) {
    let (window, mut vcx) = open_shell(cx, services_with_pickable());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());

    frame.update(&mut vcx, |f, cx| {
        f.shared_mut().set_scope(Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["A".into(), "B".into(), "C".into()],
            }],
            ..Scope::default()
        });
        cx.notify();
    });
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.run_until_parked();

    let chip = vcx.debug_bounds("scope-chip-book").expect("chip painted");
    vcx.simulate_mouse_move(
        chip.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.executor()
        .advance_clock(std::time::Duration::from_millis(600));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("tip-scope-chip-book").is_some());
    assert!(
        vcx.debug_bounds("tip-scope-chip-book-chord-mod+p")
            .is_some()
            || vcx
                .debug_bounds("tip-scope-chip-book-chord-alt+p")
                .is_some(),
        "the tooltip must name frame::pick's chord"
    );
    let title = vcx
        .debug_bounds("tip-scope-chip-book-title")
        .expect("title painted");
    assert!(
        title.size.width > chip.size.width,
        "the full list is longer than the elided chip"
    );
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
        f.shared_mut().set_scope(Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into()],
            }],
            ..Scope::default()
        });
        cx.notify();
    });
    let v0 = frame.read_with(&vcx, |f, _| f.shared().versions().scope);

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

    assert!(
        shell.read_with(&vcx, |s, _| matches!(
            s.picker.as_ref().map(|p| &p.stage),
            Some(picker::Stage::Columns)
        )),
        "escape from Values steps back to Columns first"
    );
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().ticked.len()),
        0,
        "the ticks are dropped on the way back"
    );
    let book_ix = shell.read_with(&vcx, |s, _| {
        s.pickable.iter().position(|p| p.column == "book").unwrap()
    });
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().selected),
        book_ix,
        "with the cursor on the column just left"
    );
    vcx.simulate_keystrokes("escape");
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
    assert!(
        shell.read_with(&vcx, |s, _| s.picker.is_none()),
        "and a second escape closes, clearing the picker like every other dialog"
    );
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().versions().scope),
        v0,
        "escape must not touch the scope"
    );
    let books = frame.read_with(&vcx, |f, _| {
        f.shared()
            .scope()
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

/// Clicking a Values row selects it; clicking its tick toggles it.
#[gpui::test]
fn a_values_row_click_selects_and_only_the_tick_toggles(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_pickable());
    let shell = shell_of(&window, &mut vcx);
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
    let ticked = |vcx: &gpui::VisualTestContext| {
        shell.read_with(vcx, |s, _| s.picker.as_ref().unwrap().ticked.len())
    };
    assert_eq!(ticked(&vcx), 0, "nothing pre-ticked: the scope is empty");

    // The row's label text, well right of the tick.
    let row = vcx.debug_bounds("picker-value-BK001").expect("row paints");
    vcx.simulate_click(
        gpui::point(row.origin.x + row.size.width / 2.0, row.center().y),
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().selected),
        1,
        "the row click moved the cursor"
    );
    assert_eq!(ticked(&vcx), 0, "and toggled nothing");

    let tick = vcx
        .debug_bounds("picker-tick-BK001")
        .expect("the tick paints");
    vcx.simulate_click(tick.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(ticked(&vcx), 1, "the tick click is `tab`");
}

/// A double-click on a Values row toggles its tick: the first press
/// selects, the second (`click_count == 2`) is `tab`. A second double-click
/// toggles it back, and the toggle counts as a touch, so enter applies the
/// shown ticks rather than falling back to the highlighted value.
#[gpui::test]
fn a_values_row_double_click_toggles_its_tick(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_pickable());
    let shell = shell_of(&window, &mut vcx);
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
    let ticked = |vcx: &gpui::VisualTestContext| {
        shell.read_with(vcx, |s, _| {
            s.picker
                .as_ref()
                .unwrap()
                .ticked
                .iter()
                .cloned()
                .collect::<Vec<_>>()
        })
    };

    let row = vcx.debug_bounds("picker-value-BK001").expect("row paints");
    let at = gpui::point(row.origin.x + row.size.width / 2.0, row.center().y);
    super::double_click(&mut vcx, at, gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().selected),
        1,
        "the double-click selected the row"
    );
    assert_eq!(ticked(&vcx), vec!["BK001".to_string()], "and ticked it");

    super::double_click(&mut vcx, at, gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(ticked(&vcx).is_empty(), "a second double-click unticks it");

    // The emptied set was touched: enter drops the selection instead of
    // committing the highlighted BK001.
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, cx| s
            .frame
            .read(cx)
            .shared()
            .scope()
            .dimensions
            .is_empty()),
        "enter applied the touched, empty tick set"
    );
}

/// The Values footer says `back`, the Columns footer says `close`.
#[test]
fn the_values_hint_says_escape_goes_back() {
    use crate::shell::picker::{Hint, Stage, hints};
    let values = hints(&Stage::Values {
        column: "book".into(),
    });
    let after_escape = values
        .windows(2)
        .find(|w| w[0] == Hint::Key("escape"))
        .map(|w| w[1]);
    assert_eq!(after_escape, Some(Hint::Text("back")));
    let columns = hints(&Stage::Columns);
    let after_escape = columns
        .windows(2)
        .find(|w| w[0] == Hint::Key("escape"))
        .map(|w| w[1]);
    assert_eq!(after_escape, Some(Hint::Text("close")));
}

/// Picker navigation uses shared list handling: Ctrl-D moves five rows and clamps,
/// while Up from row zero wraps.
#[gpui::test]
fn the_values_list_takes_the_full_nav_set(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_pickable());
    let shell = shell_of(&window, &mut vcx);
    dispatch_action(&shell, "frame::pick_book", &mut vcx);
    let values: Vec<(String, u64)> = (0..8).map(|i| (format!("BK00{i}"), 1)).collect();
    shell.update(&mut vcx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: PICKER_KEY,
                tag: 1,
                column: "book".into(),
                values: Ok(values),
            },
            cx,
        )
    });
    vcx.run_until_parked();
    let selected = |vcx: &gpui::VisualTestContext| {
        shell.read_with(vcx, |s, _| s.picker.as_ref().unwrap().selected)
    };
    vcx.simulate_keystrokes("ctrl-d");
    assert_eq!(selected(&vcx), 5, "ctrl+d moves five");
    vcx.simulate_keystrokes("ctrl-d");
    assert_eq!(selected(&vcx), 7, "and clamps at the end");
    vcx.simulate_keystrokes("down");
    assert_eq!(selected(&vcx), 0, "a bare down at the end wraps");
    vcx.simulate_keystrokes("up");
    assert_eq!(selected(&vcx), 7, "and a bare up at the top wraps");
}

/// Picker request tags increase across dialog openings. An outcome from a previous open
/// on the same column must not pass the new dialog's tag check.
#[gpui::test]
fn a_second_open_on_the_same_column_carries_a_larger_tag_than_the_first(
    cx: &mut gpui::TestAppContext,
) {
    let (window, mut vcx) = open_shell(cx, services_with_pickable());
    let shell = shell_of(&window, &mut vcx);

    dispatch_action(&shell, "frame::pick_book", &mut vcx);
    let first_tag = shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().tag);

    // The action opens Values directly; Escape returns to Columns, and another Escape
    // closes the dialog.
    vcx.simulate_keystrokes("escape");
    vcx.simulate_keystrokes("escape");
    assert!(
        shell.read_with(&vcx, |s, _| s.picker.is_none()),
        "escape must close the picker so the second open is a fresh one"
    );

    dispatch_action(&shell, "frame::pick_book", &mut vcx);
    let second_tag = shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().tag);

    assert!(
        second_tag > first_tag,
        "a fresh open must never repeat a tag an earlier open already used: \
         first {first_tag}, second {second_tag}"
    );
}

/// Opening the picker, selecting a column, navigating to a value, and pressing Enter
/// without ticking commits that highlighted value. Start at Columns to exercise its
/// Enter transition as well as Values commit.
#[gpui::test]
fn arrowing_to_a_value_and_pressing_enter_commits_it_without_tab(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_pickable());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    let v0 = frame.read_with(&vcx, |f, _| f.shared().versions().scope);

    dispatch_action(&shell, "frame::pick", &mut vcx);
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.picker.as_ref().map(|p| p.stage.clone())),
        Some(crate::shell::picker::Stage::Columns),
    );
    assert!(
        vcx.debug_bounds("picker-hints").is_some(),
        "the footer hint must actually paint, not merely exist as data"
    );

    // `enter` at the columns stage commits the highlighted column.
    vcx.simulate_keystrokes("enter");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.picker.as_ref().map(|p| p.stage.clone())),
        Some(crate::shell::picker::Stage::Values {
            column: "book".into()
        }),
        "enter at the columns stage must open that column's values"
    );

    let tag = shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().tag);
    shell.update(&mut vcx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: PICKER_KEY,
                tag,
                column: "book".into(),
                values: Ok(vec![
                    ("BK000".into(), 1),
                    ("BK001".into(), 2),
                    ("BK002".into(), 3),
                ]),
            },
            cx,
        )
    });
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("picker-hints").is_some(),
        "the values stage paints its own hint row too"
    );

    // Arrow to the second value and apply — no `tab`, nothing ticked.
    vcx.simulate_keystrokes("down enter");

    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().versions().scope),
        v0 + 1,
        "the highlighted value must land as one real scope change"
    );
    let books = frame.read_with(&vcx, |f, _| {
        f.shared()
            .scope()
            .dimensions
            .iter()
            .find(|d| d.column == "book")
            .expect("a book selection")
            .values
            .clone()
    });
    assert_eq!(books, vec!["BK001".to_string()]);
}

/// The other half of `ticks_touched`: an empty tick set the user *made*
/// empty still drops the column, so `ctrl+x` then `enter` remains the
/// keyboard route to clearing one dimension (PHILOSOPHY: every action
/// keyboard-reachable — the chip's close glyph is mouse-only).
#[gpui::test]
fn ctrl_x_then_enter_clears_the_columns_selection(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_pickable());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());

    frame.update(&mut vcx, |f, cx| {
        f.shared_mut().set_scope(Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into()],
            }],
            ..Scope::default()
        });
        cx.notify();
    });

    dispatch_action(&shell, "frame::pick_book", &mut vcx);
    let tag = shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().tag);
    shell.update(&mut vcx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: PICKER_KEY,
                tag,
                column: "book".into(),
                values: Ok(vec![("BK000".into(), 1), ("BK001".into(), 2)]),
            },
            cx,
        )
    });
    vcx.run_until_parked();

    vcx.simulate_keystrokes("ctrl-x enter");

    assert!(
        frame.read_with(&vcx, |f, _| f
            .shared()
            .scope()
            .dimensions
            .iter()
            .all(|d| d.column != "book")),
        "an explicitly cleared tick set drops the column, not commits the highlight"
    );
}

/// Moving beyond the visible values must scroll the list viewport with the selection.
/// Assert rendered row visibility as well as the selected index to verify
/// `sync_picker_scroll` drives the uniform-list handle.
#[gpui::test]
fn keyboard_navigation_past_visible_rows_scrolls_the_selection_into_view(
    cx: &mut gpui::TestAppContext,
) {
    let (window, mut vcx) = open_shell(cx, services_with_pickable());
    let shell = shell_of(&window, &mut vcx);

    dispatch_action(&shell, "frame::pick_book", &mut vcx);

    // 40 values — well past `VISIBLE_ROWS` (12) — so 20 `down`s below
    // both leave the old screenful and land on a row `uniform_list`
    // itself would never have painted without scroll-follow.
    let values: Vec<(String, u64)> = (0..40).map(|i| (format!("BK{i:03}"), i as u64)).collect();
    shell.update(&mut vcx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: PICKER_KEY,
                tag: 1,
                column: "book".into(),
                values: Ok(values),
            },
            cx,
        )
    });
    vcx.run_until_parked();

    let downs = vec!["down"; 20].join(" ");
    vcx.simulate_keystrokes(&downs);

    assert_eq!(
        shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().selected),
        20,
        "20 real 'down' keystrokes should advance the selection to row 20"
    );

    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });

    let list_bounds = vcx
        .debug_bounds("picker-values-list")
        .expect("the values list container should have painted");
    // Unlike the palette's plain scrollable `div` (every row always laid
    // out), `uniform_list` is genuinely virtualizing: row 20 only paints
    // at all once the viewport has scrolled to include it, so a missing
    // `debug_bounds` here is itself proof scroll-follow didn't happen —
    // not just "off-screen but present", as the palette's own version of
    // this test gets to assume.
    let row_bounds = vcx
        .debug_bounds("picker-value-BK020")
        .expect("row 20 (BK020) should have painted once scroll-follow brought it into view");
    assert!(
        list_bounds.intersects(&row_bounds),
        "row 20 {row_bounds:?} should be scrolled into the visible list \
         viewport {list_bounds:?} after the selection moved onto it, not \
         left above/below it with only its index having changed"
    );
}

/// A query typed at the Columns stage narrows columns only. `enter`
/// clears the shared Input with `set_value`, which emits no change event,
/// so the transition must reset the mirrored query itself; otherwise the
/// column filter text filters the new column's values and hides them.
#[gpui::test]
fn a_column_filter_does_not_carry_into_the_values_stage(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_pickable());
    let shell = shell_of(&window, &mut vcx);

    dispatch_action(&shell, "frame::pick", &mut vcx);
    vcx.run_until_parked();
    vcx.simulate_input("bo");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().query.clone()),
        "bo",
        "the typed filter must reach the picker"
    );
    vcx.simulate_keystrokes("enter");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().query.clone()),
        "",
        "entering Values must start with an empty query"
    );

    let tag = shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().tag);
    shell.update(&mut vcx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: PICKER_KEY,
                tag,
                column: "book".into(),
                values: Ok(vec![("BK000".into(), 1), ("XX".into(), 2)]),
            },
            cx,
        )
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("picker-value-BK000").is_some());
    assert!(vcx.debug_bounds("picker-value-XX").is_some());
}

/// The Back button returns Values to Columns through `escape`'s own step, and paints
/// only while there is a Values stage to leave. Typing afterwards lands in the still
/// focused filter, and `enter` reopens the column the cursor was returned to.
#[gpui::test]
fn the_back_button_returns_values_to_columns(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_pickable());
    let shell = shell_of(&window, &mut vcx);
    dispatch_action(&shell, "frame::pick_book", &mut vcx);
    vcx.run_until_parked();
    let back = vcx
        .debug_bounds("shell-modal-back")
        .expect("the Values stage paints a Back button");
    vcx.simulate_click(back.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.picker.as_ref().map(|p| p.stage.clone())),
        Some(picker::Stage::Columns)
    );
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        vcx.debug_bounds("shell-modal-back").is_none(),
        "Columns is the first screen"
    );

    vcx.simulate_input("bo");
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().query.clone()),
        "bo",
        "the filter still hears the keyboard"
    );
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(matches!(
        shell.read_with(&vcx, |s, _| s.picker.as_ref().map(|p| p.stage.clone())),
        Some(picker::Stage::Values { ref column }) if column == "book"
    ));
}
