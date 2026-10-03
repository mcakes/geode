//! The Scope dialog: the lane's scope by ingredient (Current), with steps
//! for each ingredient pushed over it.

use super::*;
use geode_core::scope::{DimensionSelection, Scope, parse_expr};

/// `book` and `desk` are dimensions; `npv` and `delta` measures; one
/// saved scope `eu` (book BK001) and one named expression `liq`.
pub(super) fn services() -> ShellServices {
    let mut services = test_services();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            LayerDoc::builtin(
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.desk]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
                 [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                 [risk.columns.delta]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
            )
            .unwrap(),
            LayerDoc::builtin("scopes", "[eu]\n[eu.dimensions]\nbook = [\"BK001\"]\n").unwrap(),
            LayerDoc::builtin("expressions", "[liq]\nexpression = \"npv > 0\"\n").unwrap(),
        ],
        ..ConfigSources::default()
    });
    services
}

pub(super) fn frame_of(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> Entity<Frame> {
    shell.read_with(cx, |s, _| s.frame().clone())
}

pub(super) fn draw(cx: &mut gpui::VisualTestContext) {
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

/// A shell whose shared lane holds `scope`, with the Scope dialog open
/// through its door and drawn once.
pub(super) fn open_on(
    cx: &mut gpui::TestAppContext,
    scope: Scope,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    let (window, mut vcx) = open_shell(cx, services());
    let shell = shell_of(&window, &mut vcx);
    frame_of(&shell, &vcx).update(&mut vcx, |f, cx| {
        f.shared_mut().set_scope(scope);
        cx.notify();
    });
    vcx.run_until_parked();
    dispatch_action(&shell, "frame::scope", &mut vcx);
    draw(&mut vcx);
    (shell, vcx)
}

pub(super) fn rich_scope() -> Scope {
    Scope {
        dimensions: vec![DimensionSelection {
            column: "book".into(),
            values: vec!["BK001".into(), "BK002".into()],
        }],
        text: Some("dec".into()),
        expression: Some(parse_expr("npv > 0 and delta < 5").unwrap()),
        impossible: false,
        named: vec!["liq".into()],
    }
}

#[gpui::test]
fn mod_o_opens_the_scope_dialog_on_current(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services());
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("alt-o");
    draw(&mut vcx);
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::Scope)
    );
    assert!(vcx.debug_bounds("scope-dialog").is_some());
    for section in ["dimensions", "expressions", "text"] {
        assert!(
            bounds(&mut vcx, format!("scope-dialog-section-{section}")).is_some(),
            "{section} header paints"
        );
        assert!(
            bounds(&mut vcx, format!("scope-dialog-empty-{section}")).is_some(),
            "{section} empty row paints"
        );
    }
}

#[gpui::test]
fn each_ingredient_is_a_painted_row_in_section_order(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, rich_scope());
    let ids = shell.read_with(&vcx, |s, _| {
        s.scope_dialog
            .as_ref()
            .unwrap()
            .rows
            .rows
            .iter()
            .map(|r| r.id.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(ids.len(), 5, "{ids:?}");
    for i in 0..5 {
        assert!(
            bounds(&mut vcx, format!("scope-dialog-row-{i}")).is_some(),
            "row {i} paints"
        );
    }
    assert!(vcx.debug_bounds("scope-dialog-empty-dimensions").is_none());
    let mut row = |i: usize| bounds(&mut vcx, format!("scope-dialog-row-{i}")).unwrap();
    assert!(row(0).top() < row(1).top() && row(3).top() < row(4).top());
}

#[gpui::test]
fn the_title_says_where_the_scope_came_from(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, Scope::default());
    assert_eq!(
        title_extra(&shell, &vcx),
        None,
        "an empty scope says nothing"
    );
    frame_of(&shell, &vcx).update(&mut vcx, |f, cx| {
        f.shared_mut().load_scope("eu").unwrap();
        cx.notify();
    });
    draw(&mut vcx);
    assert_eq!(title_extra(&shell, &vcx).as_deref(), Some("from eu"));
    frame_of(&shell, &vcx).update(&mut vcx, |f, cx| {
        f.shared_mut().set_text(Some("x".into()));
        cx.notify();
    });
    draw(&mut vcx);
    assert_eq!(
        title_extra(&shell, &vcx).as_deref(),
        Some("from eu, changed")
    );
    assert!(vcx.debug_bounds("scope-dialog-title-extra").is_some());
}

#[gpui::test]
fn a_contradiction_paints_its_line(cx: &mut gpui::TestAppContext) {
    let (_shell, mut vcx) = open_on(
        cx,
        Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec![],
            }],
            impossible: true,
            ..Scope::default()
        },
    );
    assert!(vcx.debug_bounds("scope-dialog-contradiction").is_some());
}

#[gpui::test]
fn rows_follow_the_frame_while_open(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, Scope::default());
    frame_of(&shell, &vcx).update(&mut vcx, |f, cx| {
        f.shared_mut().set_text(Some("spx".into()));
        cx.notify();
    });
    vcx.run_until_parked();
    // A stale prepared list panics in `build` (debug); drawing proves it is current.
    draw(&mut vcx);
    assert!(vcx.debug_bounds("scope-dialog-row-0").is_some());
    assert!(vcx.debug_bounds("scope-dialog-empty-text").is_none());
}

#[gpui::test]
fn escape_closes_the_dialog(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_on(cx, rich_scope());
    vcx.simulate_keystrokes("escape");
    assert_eq!(shell.read_with(&vcx, |s, _| s.top_kind()), None);
}

fn title_extra(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> Option<String> {
    shell.read_with(cx, |s, _| {
        s.scope_dialog
            .as_ref()
            .and_then(|d| d.title.as_ref().map(|t| t.to_string()))
    })
}

/// `debug_bounds` takes a `'static` selector; tests leak the few they format.
fn bounds(
    vcx: &mut gpui::VisualTestContext,
    selector: String,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    vcx.debug_bounds(Box::leak(selector.into_boxed_str()))
}
