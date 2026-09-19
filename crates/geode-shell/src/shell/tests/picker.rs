//! The dimension picker (Phase 4a §3.3, §3.4): the real key-dispatch and
//! `Request::Distinct` pipeline, end to end — `shell::picker`'s own
//! `mod tests` covers the pure core, this covers the wiring: opening
//! through `frame::pick_<column>`, the emitted `ShellEvent::
//! DistinctRequested`, `ShellView::deliver_distinct`'s stale-tag guard,
//! and the keyboard vocabulary (`tab`/`ctrl+a`/`ctrl+x`/`enter`/`escape`)
//! driven through the same `GeodeModal` reclaims (`dialog::
//! init_reclaimed_keybindings`) production uses.

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
fn services_with_pickable() -> ShellServices {
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

/// Fix round 1, Finding 3: the scope chip's body click (`toolbar::
/// on_chip_open`, wired in `render.rs`) had no test proving it actually
/// opens the picker — only the close glyph's `on_chip_close` was
/// e2e-covered. A real mouse click on `debug_bounds("scope-chip-book")`'s
/// centre must land the picker on `Stage::Values { column: "book" }` and
/// submit a real `DistinctRequested` for it, the same proof the keyed
/// `frame::pick_book` action gets in the test above.
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
        f.set_scope(Scope {
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
        shell.read_with(&vcx, |s, _| s.modal.is_some()),
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

/// Task 3 (tooltips): hovering a scope chip's body shows the full,
/// un-elided selection (`Chip::full`) — not the elided `summary` painted
/// on the bar — and names `frame::pick`'s chord. `services_with_pickable`
/// is used only to keep this fixture identical in shape to the click
/// test above; the picker action need not fire for a tooltip.
#[gpui::test]
fn hovering_a_scope_chip_shows_the_full_selection_and_the_picker_chord(
    cx: &mut gpui::TestAppContext,
) {
    let (window, mut vcx) = open_shell(cx, services_with_pickable());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());

    frame.update(&mut vcx, |f, cx| {
        f.set_scope(Scope {
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

    assert!(
        shell.read_with(&vcx, |s, _| matches!(
            s.picker.as_ref().map(|p| &p.stage),
            Some(picker::Stage::Columns)
        )),
        "spec §20.2: escape from Values steps back to Columns first"
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
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
    assert!(
        shell.read_with(&vcx, |s, _| s.picker.is_none()),
        "and a second escape closes, clearing the picker like every other dialog"
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

/// §20.3's split applied here: a click on a Values row SELECTS it; the
/// tick glyph is the click target that toggles, as it is on the object
/// dialog's list rows. Until now the whole row toggled on one click.
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

/// Spec §20.5 on the picker: `ctrl+d` moves five and clamps, `up` at
/// row 0 wraps — the same `nav_command` + `apply` pair every other list
/// routes through, replacing the picker's own four-key `nav_delta`.
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

/// Phase 4b M5: `PickerState::tag` used to reset to the same starting
/// value on every `open`, so a stale `DistinctOutcome` from a first open
/// on `book` could pass the tag check of a second, unrelated open on the
/// same column (both opens' one `request_values` call bumped a fresh
/// `PickerState`'s tag from 0 to 1). `ShellView::next_picker_tag` is now
/// the session-wide source of every tag, so a second open's request
/// always carries a strictly larger tag than the first's.
#[gpui::test]
fn a_second_open_on_the_same_column_carries_a_larger_tag_than_the_first(
    cx: &mut gpui::TestAppContext,
) {
    let (window, mut vcx) = open_shell(cx, services_with_pickable());
    let shell = shell_of(&window, &mut vcx);

    dispatch_action(&shell, "frame::pick_book", &mut vcx);
    let first_tag = shell.read_with(&vcx, |s, _| s.picker.as_ref().unwrap().tag);

    // `frame::pick_book` opens straight into `Values` (spec §20.2's first
    // `escape` steps back to `Columns`; the second closes).
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

/// The single-value flow, end to end through the real key pipeline:
/// `alt+p`, a column, arrow to a value, `enter` — no `tab` anywhere. This
/// used to close the modal having changed nothing, because `apply` read
/// an empty tick set as "select nothing" (see `PickerState::
/// ticks_touched`). Driven from `frame::pick` at the `Columns` stage
/// rather than `frame::pick_book`, since the columns stage's own `enter`
/// had no end-to-end coverage at all.
#[gpui::test]
fn arrowing_to_a_value_and_pressing_enter_commits_it_without_tab(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_pickable());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    let v0 = frame.read_with(&vcx, |f, _| f.versions().scope);

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

    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.versions().scope),
        v0 + 1,
        "the highlighted value must land as one real scope change"
    );
    let books = frame.read_with(&vcx, |f, _| {
        f.scope()
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
        f.set_scope(Scope {
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
            .scope()
            .dimensions
            .iter()
            .all(|d| d.column != "book")),
        "an explicitly cleared tick set drops the column, not commits the highlight"
    );
}

/// Fix round 1, Finding 1: keyboard navigation past `palette::
/// VISIBLE_ROWS` (12) must scroll the values `uniform_list`'s viewport
/// to follow `selected` (`ShellView::picker_scroll`, driven by
/// `picker::sync_picker_scroll`) — the same "prove the viewport actually
/// followed, not just that an index changed" standard `palette`'s own
/// `arrow_down_past_visible_rows_advances_selection_and_scrolls_it_into_
/// view` test holds itself to, copied here for the picker's `uniform_
/// list` (a different gpui scroll-follow type — see that module's doc
/// comment — but the same observable contract).
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
