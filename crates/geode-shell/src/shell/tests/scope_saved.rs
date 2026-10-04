//! The Scope dialog's Saved screen: saved scopes then saved expressions,
//! `enter` loading a scope or toggling an expression, the filter, and the
//! doors that open the screen.

use super::*;
use geode_core::scope::Scope;
use geode_core::scopes::SavedScopes;

use super::scope_dialog::{draw, frame_of};

/// `book` a dimension and `npv` a measure; saved scopes `eu` (book BK001)
/// and `asia` (book BK002); expressions `liq` (`npv > 0`) and `big`
/// (`npv > 100`).
fn services() -> ShellServices {
    let mut services = test_services();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            LayerDoc::builtin(
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
                 [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
            )
            .unwrap(),
            LayerDoc::builtin(
                "scopes",
                "[eu]\n[eu.dimensions]\nbook = [\"BK001\"]\n\
                 [asia]\n[asia.dimensions]\nbook = [\"BK002\"]\n",
            )
            .unwrap(),
            LayerDoc::builtin(
                "expressions",
                "[liq]\nexpression = \"npv > 0\"\n[big]\nexpression = \"npv > 100\"\n",
            )
            .unwrap(),
        ],
        ..ConfigSources::default()
    });
    services
}

fn liq_scope() -> Scope {
    Scope {
        named: vec!["liq".into()],
        ..Scope::default()
    }
}

/// A shell whose lane names `liq`, with nothing open yet.
fn shell_on_liq(cx: &mut gpui::TestAppContext) -> (Entity<ShellView>, gpui::VisualTestContext) {
    let (window, mut vcx) = open_shell(cx, services());
    let shell = shell_of(&window, &mut vcx);
    frame_of(&shell, &vcx).update(&mut vcx, |f, cx| {
        f.shared_mut().set_scope(liq_scope());
        f.shared_mut().clear_history();
        cx.notify();
    });
    vcx.run_until_parked();
    draw(&mut vcx);
    (shell, vcx)
}

/// The Scope dialog on Current, then `o` to the Saved screen, drawn.
fn open_saved_from_current(
    cx: &mut gpui::TestAppContext,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    let (shell, mut vcx) = shell_on_liq(cx);
    dispatch_action(&shell, "frame::scope", &mut vcx);
    draw(&mut vcx);
    vcx.simulate_keystrokes("o");
    draw(&mut vcx);
    (shell, vcx)
}

fn lane_scope(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> Scope {
    frame_of(shell, cx).read_with(cx, |f, _| f.shared().scope().clone())
}

fn top_layer(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> Option<Layer> {
    shell.read_with(cx, |s, _| {
        s.scope_dialog.as_ref().map(|d| d.layers.top().clone())
    })
}

fn depth(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> usize {
    shell.read_with(cx, |s, _| {
        s.scope_dialog.as_ref().map_or(0, |d| d.layers.depth())
    })
}

/// The Saved screen's visible rows' names, in painted order.
fn visible_names(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> Vec<String> {
    shell.read_with(cx, |s, _| {
        let saved = &s.scope_dialog.as_ref().expect("dialog open").saved;
        saved
            .visible
            .iter()
            .map(|&i| saved.rows[i].name.clone())
            .collect()
    })
}

fn error(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> Option<String> {
    shell.read_with(cx, |s, _| {
        s.scope_dialog.as_ref().and_then(|d| d.error.clone())
    })
}

fn bounds(
    vcx: &mut gpui::VisualTestContext,
    selector: String,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    vcx.debug_bounds(Box::leak(selector.into_boxed_str()))
}

use crate::shell::scopedialog::state::Layer;

#[gpui::test]
fn o_opens_saved_with_scopes_then_expressions_in_name_order(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_saved_from_current(cx);
    assert_eq!(top_layer(&shell, &vcx), Some(Layer::Saved));
    assert_eq!(depth(&shell, &vcx), 2, "Saved is pushed over Current");
    assert_eq!(visible_names(&shell, &vcx), ["asia", "eu", "big", "liq"]);
    assert!(vcx.debug_bounds("scope-saved").is_some());
    for section in ["scopes", "expressions"] {
        assert!(bounds(&mut vcx, format!("scope-saved-section-{section}")).is_some());
        assert!(bounds(&mut vcx, format!("scope-saved-note-{section}")).is_some());
        assert!(bounds(&mut vcx, format!("scope-saved-empty-{section}")).is_none());
    }
    let tops: Vec<_> = (0..4)
        .map(|i| {
            bounds(&mut vcx, format!("scope-saved-row-{i}"))
                .expect("row paints")
                .top()
        })
        .collect();
    assert!(tops.windows(2).all(|w| w[0] < w[1]), "{tops:?}");
    let expressions = vcx
        .debug_bounds("scope-saved-section-expressions")
        .unwrap()
        .top();
    assert!(tops[1] < expressions && expressions < tops[2]);
    assert!(
        vcx.debug_bounds("scope-saved-tag-applied-3").is_some(),
        "liq is applied"
    );
    assert!(vcx.debug_bounds("scope-saved-tag-applied-2").is_none());
}

#[gpui::test]
fn an_empty_section_paints_its_empty_row(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = shell_on_liq(cx);
    frame_of(&shell, &vcx).update(&mut vcx, |f, cx| {
        f.replace_saved_scopes(SavedScopes::new());
        cx.notify();
    });
    vcx.run_until_parked();
    dispatch_action(&shell, "frame::scope_saved", &mut vcx);
    draw(&mut vcx);
    assert!(vcx.debug_bounds("scope-saved-empty-scopes").is_some());
    assert!(vcx.debug_bounds("scope-saved-empty-expressions").is_none());
    assert_eq!(visible_names(&shell, &vcx), ["big", "liq"]);
}

#[gpui::test]
fn enter_on_a_scope_loads_it_and_returns_to_current(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_saved_from_current(cx);
    vcx.simulate_keystrokes("j enter"); // eu
    draw(&mut vcx);
    assert_eq!(lane_scope(&shell, &vcx).sole("book"), Some("BK001"));
    assert_eq!(
        frame_of(&shell, &vcx).read_with(&vcx, |f, _| f.shared().loaded_from().map(str::to_string)),
        Some("eu".to_string())
    );
    assert_eq!(top_layer(&shell, &vcx), Some(Layer::Current));
    vcx.simulate_keystrokes("u");
    assert_eq!(lane_scope(&shell, &vcx), liq_scope(), "one undo restores");
}

#[gpui::test]
fn enter_on_an_expression_toggles_it_and_returns(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_saved_from_current(cx);
    vcx.simulate_keystrokes("j j enter"); // big
    draw(&mut vcx);
    assert_eq!(lane_scope(&shell, &vcx).named, ["liq", "big"]);
    assert_eq!(top_layer(&shell, &vcx), Some(Layer::Current));
    vcx.simulate_keystrokes("o");
    draw(&mut vcx);
    vcx.simulate_keystrokes("k enter"); // wraps to liq
    draw(&mut vcx);
    assert_eq!(lane_scope(&shell, &vcx).named, ["big"]);
    assert_eq!(top_layer(&shell, &vcx), Some(Layer::Current));
}

#[gpui::test]
fn the_load_glyph_opens_saved_alone_and_enter_closes(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = shell_on_liq(cx);
    assert!(vcx.debug_bounds("scope-load-chip-open").is_none());
    let glyph = vcx
        .debug_bounds("scope-load-chip")
        .expect("the load glyph paints");
    vcx.simulate_click(glyph.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    draw(&mut vcx);
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::Scope)
    );
    assert_eq!(top_layer(&shell, &vcx), Some(Layer::Saved));
    assert_eq!(depth(&shell, &vcx), 1, "Saved is the bottom layer");
    assert!(
        vcx.debug_bounds("scope-load-chip-open").is_some(),
        "the glyph reads pressed while Saved is up"
    );
    vcx.simulate_keystrokes("enter"); // asia
    vcx.run_until_parked();
    draw(&mut vcx);
    assert_eq!(shell.read_with(&vcx, |s, _| s.top_kind()), None);
    assert_eq!(lane_scope(&shell, &vcx).sole("book"), Some("BK002"));
    assert!(vcx.debug_bounds("scope-load-chip-open").is_none());
}

#[gpui::test]
fn escape_from_saved_alone_closes_and_from_current_returns(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_saved_from_current(cx);
    vcx.simulate_keystrokes("escape");
    assert_eq!(top_layer(&shell, &vcx), Some(Layer::Current));
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::Scope)
    );
    vcx.simulate_keystrokes("escape");
    assert_eq!(shell.read_with(&vcx, |s, _| s.top_kind()), None);

    dispatch_action(&shell, "frame::scope_saved", &mut vcx);
    draw(&mut vcx);
    vcx.simulate_keystrokes("escape");
    assert_eq!(shell.read_with(&vcx, |s, _| s.top_kind()), None);
    assert_eq!(lane_scope(&shell, &vcx), liq_scope());
}

#[gpui::test]
fn config_scopes_and_config_expressions_open_saved(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = shell_on_liq(cx);
    for action in [
        "config::scopes",
        "config::expressions",
        "frame::scope_saved",
    ] {
        dispatch_action(&shell, action, &mut vcx);
        draw(&mut vcx);
        assert_eq!(
            shell.read_with(&vcx, |s, _| s.top_kind()),
            Some(dialog::DialogKind::Scope),
            "{action}"
        );
        assert_eq!(top_layer(&shell, &vcx), Some(Layer::Saved), "{action}");
        assert_eq!(depth(&shell, &vcx), 1, "{action}");
        assert!(vcx.debug_bounds("scope-saved").is_some(), "{action}");
        vcx.simulate_keystrokes("escape");
        assert_eq!(shell.read_with(&vcx, |s, _| s.top_kind()), None, "{action}");
    }
}

#[gpui::test]
fn the_filter_keeps_sections_and_escape_restores(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_saved_from_current(cx);
    vcx.simulate_keystrokes("/");
    vcx.simulate_input("i");
    vcx.run_until_parked();
    draw(&mut vcx);
    assert_eq!(visible_names(&shell, &vcx), ["asia", "big", "liq"]);
    assert!(vcx.debug_bounds("scope-saved-row-2").is_some());
    assert!(vcx.debug_bounds("scope-saved-row-3").is_none());
    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);
    let (query, mode) = shell.read_with(&vcx, |s, _| {
        let saved = &s.scope_dialog.as_ref().unwrap().saved;
        (saved.query.clone(), saved.mode)
    });
    assert_eq!(query, "");
    assert_eq!(mode, crate::dialogmode::DialogMode::Normal);
    assert_eq!(visible_names(&shell, &vcx), ["asia", "eu", "big", "liq"]);
    assert_eq!(
        top_layer(&shell, &vcx),
        Some(Layer::Saved),
        "escape only left the filter"
    );

    vcx.simulate_keystrokes("/");
    vcx.simulate_input("x");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    draw(&mut vcx);
    let (query, mode) = shell.read_with(&vcx, |s, _| {
        let saved = &s.scope_dialog.as_ref().unwrap().saved;
        (saved.query.clone(), saved.mode)
    });
    assert_eq!(query, "x");
    assert_eq!(mode, crate::dialogmode::DialogMode::Normal);
    assert_eq!(top_layer(&shell, &vcx), Some(Layer::Saved));
    assert_eq!(lane_scope(&shell, &vcx), liq_scope(), "nothing loaded");
}

#[gpui::test]
fn clicking_the_filter_row_enters_filter_mode(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_saved_from_current(cx);
    let row = vcx
        .debug_bounds("scope-saved-filter")
        .expect("the filter row paints");
    vcx.simulate_click(row.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    vcx.simulate_input("eu");
    vcx.run_until_parked();
    draw(&mut vcx);
    assert_eq!(visible_names(&shell, &vcx), ["eu"]);
}

#[gpui::test]
fn a_row_double_click_commits_it(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_saved_from_current(cx);
    let row = vcx.debug_bounds("scope-saved-row-2").expect("big paints");
    double_click(&mut vcx, row.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(lane_scope(&shell, &vcx).named, ["liq", "big"]);
    assert_eq!(top_layer(&shell, &vcx), Some(Layer::Current));
}

#[gpui::test]
fn entering_a_vanished_scope_says_so(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_saved_from_current(cx);
    frame_of(&shell, &vcx).update(&mut vcx, |f, _| {
        let mut saved: SavedScopes = f.saved_scopes().clone();
        saved.remove("asia");
        assert!(f.replace_saved_scopes(saved));
    });
    vcx.simulate_keystrokes("enter"); // row 0 was asia
    draw(&mut vcx);
    assert_eq!(
        error(&shell, &vcx).as_deref(),
        Some("that saved scope no longer exists")
    );
    assert!(vcx.debug_bounds("scope-dialog-error").is_some());
    assert_eq!(lane_scope(&shell, &vcx), liq_scope());
    assert_eq!(top_layer(&shell, &vcx), Some(Layer::Saved));
    assert_eq!(
        visible_names(&shell, &vcx),
        ["eu", "big", "liq"],
        "re-derived"
    );
}

#[gpui::test]
fn entering_a_vanished_expression_says_so(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_saved_from_current(cx);
    vcx.simulate_keystrokes("j j"); // big
    // Every definition gone without a refresh: the row under the cursor is
    // the one derived before.
    frame_of(&shell, &vcx).update(&mut vcx, |f, _| {
        let named = geode_core::named::NamedExpressions::default();
        assert!(f.replace_named_expressions(named));
    });
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert_eq!(
        error(&shell, &vcx).as_deref(),
        Some("that expression no longer exists")
    );
    assert_eq!(lane_scope(&shell, &vcx), liq_scope());
    assert_eq!(top_layer(&shell, &vcx), Some(Layer::Saved));
}

#[gpui::test]
fn e_and_n_on_a_scope_row_refuse_with_the_way_to_do_it(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_saved_from_current(cx);
    vcx.simulate_keystrokes("e");
    draw(&mut vcx);
    assert_eq!(
        error(&shell, &vcx).as_deref(),
        Some("load it, change it, then save over it (s)")
    );
    assert!(vcx.debug_bounds("scope-dialog-error").is_some());
    vcx.simulate_keystrokes("n");
    draw(&mut vcx);
    assert_eq!(
        error(&shell, &vcx).as_deref(),
        Some("narrow the current scope, then save it (s)")
    );
    vcx.simulate_keystrokes("j");
    assert_eq!(error(&shell, &vcx), None, "a claimed key drops the refusal");
    assert_eq!(top_layer(&shell, &vcx), Some(Layer::Saved));
    assert_eq!(lane_scope(&shell, &vcx), liq_scope());
}
