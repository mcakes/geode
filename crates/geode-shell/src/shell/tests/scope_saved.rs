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

    // `b` ranks `big` (a prefix) above both scopes; the rows stay in row
    // order all the same, scopes first.
    vcx.simulate_keystrokes("/");
    vcx.simulate_input("b");
    vcx.run_until_parked();
    assert_eq!(visible_names(&shell, &vcx), ["asia", "eu", "big"]);
    vcx.simulate_keystrokes("escape");

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

/// The pointer route of `escape` from Saved over Current: the title row's
/// Back button returns to Current, leaving a filter on the way, in one click.
#[gpui::test]
fn the_back_button_returns_from_saved_to_current(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_saved_from_current(cx);
    vcx.simulate_keystrokes("/");
    vcx.simulate_input("eu");
    vcx.run_until_parked();
    draw(&mut vcx);
    let back = vcx
        .debug_bounds("shell-modal-back")
        .expect("Saved over Current paints Back");
    vcx.simulate_click(back.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    draw(&mut vcx);
    assert_eq!(top_layer(&shell, &vcx), Some(Layer::Current));
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.top_kind()),
        Some(dialog::DialogKind::Scope)
    );
    assert!(vcx.debug_bounds("scope-saved").is_none());
    assert!(vcx.debug_bounds("scope-dialog").is_some());
    assert!(
        vcx.debug_bounds("shell-modal-back").is_none(),
        "Current is the first screen"
    );
    assert_eq!(lane_scope(&shell, &vcx), liq_scope(), "nothing loaded");
}

/// Saved opened alone has no screen behind it: no Back button.
#[gpui::test]
fn saved_opened_alone_paints_no_back_button(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = shell_on_liq(cx);
    let glyph = vcx
        .debug_bounds("scope-load-chip")
        .expect("the load glyph paints");
    vcx.simulate_click(glyph.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    draw(&mut vcx);
    assert_eq!(top_layer(&shell, &vcx), Some(Layer::Saved));
    assert!(vcx.debug_bounds("scope-saved").is_some());
    assert!(vcx.debug_bounds("shell-modal-back").is_none());
}

// ---- The save prompt -------------------------------------------------

/// A user layer defining `eu` (book BK001) and a desk layer defining
/// `desk_eu` (book BK003), over `services`' `risk` dataset and builtin
/// expressions `liq` (`npv > 0`) and `big` (`npv > 100`), with a writable
/// user directory the saves land in.
struct SaveFixture {
    _desk: tempfile::TempDir,
    user: tempfile::TempDir,
    shell: Entity<ShellView>,
    vcx: gpui::VisualTestContext,
}

fn save_fixture(cx: &mut gpui::TestAppContext) -> SaveFixture {
    save_fixture_with(cx, "")
}

/// [`save_fixture`] with `user_expressions` as the user layer's
/// `expressions.toml` when it is not empty.
fn save_fixture_with(cx: &mut gpui::TestAppContext, user_expressions: &str) -> SaveFixture {
    let desk = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    if !user_expressions.is_empty() {
        std::fs::write(user.path().join("expressions.toml"), user_expressions).unwrap();
    }
    std::fs::write(
        desk.path().join("scopes.toml"),
        "config_version = 1\n[desk_eu]\n[desk_eu.dimensions]\nbook = [\"BK003\"]\n",
    )
    .unwrap();
    std::fs::write(
        user.path().join("scopes.toml"),
        "config_version = 1\n[eu]\n[eu.dimensions]\nbook = [\"BK001\"]\n",
    )
    .unwrap();
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
                "expressions",
                "[liq]\nexpression = \"npv > 0\"\n[big]\nexpression = \"npv > 100\"\n",
            )
            .unwrap(),
        ],
        desk: Some(desk.path().to_path_buf()),
        user: Some(user.path().to_path_buf()),
    });
    let (window, mut vcx) = open_shell_with_user_dir(cx, services, user.path());
    let shell = shell_of(&window, &mut vcx);
    draw(&mut vcx);
    SaveFixture {
        _desk: desk,
        user,
        shell,
        vcx,
    }
}

fn book(value: &str) -> Scope {
    Scope {
        dimensions: vec![geode_core::scope::DimensionSelection {
            column: "book".into(),
            values: vec![value.into()],
        }],
        ..Scope::default()
    }
}

impl SaveFixture {
    fn set_lane(&mut self, scope: Scope) {
        frame_of(&self.shell, &self.vcx).update(&mut self.vcx, |f, cx| {
            f.shared_mut().set_scope(scope);
            cx.notify();
        });
        self.vcx.run_until_parked();
        draw(&mut self.vcx);
    }

    fn load(&mut self, name: &str) {
        let name = name.to_string();
        self.shell
            .update(&mut self.vcx, |s, cx| s.load_saved_scope(&name, cx))
            .expect("the fixture defines it");
        self.vcx.run_until_parked();
        draw(&mut self.vcx);
    }

    fn keys(&mut self, keys: &str) {
        self.vcx.simulate_keystrokes(keys);
        self.vcx.run_until_parked();
        draw(&mut self.vcx);
    }

    fn type_text(&mut self, text: &str) {
        self.vcx.simulate_input(text);
        self.vcx.run_until_parked();
        draw(&mut self.vcx);
    }

    fn open_current(&mut self) {
        dispatch_action(&self.shell, "frame::scope", &mut self.vcx);
        draw(&mut self.vcx);
    }

    fn flush(&mut self) {
        self.vcx.executor().advance_clock(
            crate::shell::objectdialog::apply::WRITE_DEBOUNCE
                + std::time::Duration::from_millis(10),
        );
        self.vcx.run_until_parked();
        draw(&mut self.vcx);
    }

    fn user_scopes(&self) -> String {
        std::fs::read_to_string(self.user.path().join("scopes.toml")).unwrap_or_default()
    }

    fn draft(&self) -> Option<String> {
        self.shell.read_with(&self.vcx, |s, _| {
            s.scope_dialog
                .as_ref()
                .and_then(|d| d.prompt.as_ref().map(|p| p.draft.clone()))
        })
    }

    fn prompt_error(&self) -> Option<String> {
        self.shell.read_with(&self.vcx, |s, _| {
            s.scope_dialog
                .as_ref()
                .and_then(|d| d.prompt.as_ref().and_then(|p| p.error.clone()))
        })
    }

    fn input_text(&self) -> String {
        self.shell.read_with(&self.vcx, |s, cx| {
            s.dialog_input.read(cx).value().to_string()
        })
    }

    fn top(&self) -> Option<Layer> {
        top_layer(&self.shell, &self.vcx)
    }

    fn top_kind(&self) -> Option<dialog::DialogKind> {
        self.shell.read_with(&self.vcx, |s, _| s.top_kind())
    }

    fn painted(&mut self, selector: &'static str) -> bool {
        self.vcx.debug_bounds(selector).is_some()
    }

    fn loaded_from(&self) -> Option<String> {
        frame_of(&self.shell, &self.vcx).read_with(&self.vcx, |f, _| {
            f.shared().loaded_from().map(str::to_string)
        })
    }

    fn has_saved(&self, name: &str) -> bool {
        frame_of(&self.shell, &self.vcx)
            .read_with(&self.vcx, |f, _| f.saved_scopes().contains_key(name))
    }

    fn notice(&self) -> Option<String> {
        self.shell
            .read_with(&self.vcx, |s, _| s.notice.as_ref().map(|n| n.to_string()))
    }

    fn queued(&self) -> bool {
        self.shell
            .read_with(&self.vcx, |s, _| s.pending_config_write.is_some())
    }

    fn title(&self) -> Option<String> {
        self.shell.read_with(&self.vcx, |s, _| {
            s.scope_dialog
                .as_ref()
                .and_then(|d| d.title.as_ref().map(|t| t.to_string()))
        })
    }
}

fn save_step() -> Option<Layer> {
    Some(Layer::Step(
        crate::shell::scopedialog::state::Step::SaveScope,
    ))
}

/// `s` seeds the field with where the scope came from; `enter` on a name
/// the user already holds asks first, and `y` overwrites it.
#[gpui::test]
fn s_seeds_the_source_and_enter_on_a_user_scope_asks_then_overwrites(
    cx: &mut gpui::TestAppContext,
) {
    let mut f = save_fixture(cx);
    f.load("eu");
    f.open_current();
    f.keys("t");
    f.type_text("spx");
    f.keys("enter");
    assert_eq!(f.title().as_deref(), Some("from eu, changed"));
    f.keys("s");
    assert_eq!(f.top(), save_step());
    assert_eq!(f.draft().as_deref(), Some("eu"));
    assert_eq!(f.input_text(), "eu", "the field mirrors the draft");
    assert!(f.painted("scope-dialog-name-field"));
    f.keys("enter");
    assert!(f.painted("scope-dialog-confirm"), "a user scope asks first");
    assert!(f.painted("scope-dialog-confirm-yes"));
    assert!(!f.queued(), "nothing queued before the answer");
    f.keys("y");
    assert!(!f.painted("scope-dialog-confirm"));
    assert_eq!(f.top(), Some(Layer::Current));
    assert_eq!(f.title().as_deref(), Some("from eu"));
    f.flush();
    let written = f.user_scopes();
    assert!(
        written.contains("[eu") && written.contains("spx"),
        "{written}"
    );
}

/// A new name writes at once, without a question, and the frame resolves it
/// and records it as the lane's source before the flush.
#[gpui::test]
fn saving_under_a_new_name_writes_it_and_records_the_source(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.load("eu");
    f.open_current();
    f.keys("s");
    f.keys("backspace backspace");
    assert_eq!(f.draft().as_deref(), Some(""));
    f.type_text("mine");
    // Dispatched and read in one update: no task runs between them, so the
    // batch has not flushed and only the save's own refresh can resolve it.
    let frame = frame_of(&f.shell, &f.vcx);
    let resolved = f.vcx.update(|window, cx| {
        window.dispatch_keystroke(gpui::Keystroke::parse("enter").unwrap(), cx);
        frame.read(cx).saved_scopes().contains_key("mine")
    });
    assert!(resolved, "resolved before the flush");
    f.vcx.run_until_parked();
    draw(&mut f.vcx);
    assert!(!f.painted("scope-dialog-confirm"), "a new name never asks");
    assert!(f.has_saved("mine"));
    assert_eq!(f.loaded_from().as_deref(), Some("mine"));
    assert_eq!(f.top(), Some(Layer::Current));
    assert_eq!(f.title().as_deref(), Some("from mine"));
    f.flush();
    let written = f.user_scopes();
    assert!(
        written.contains("[mine") && written.contains("BK001"),
        "{written}"
    );
}

/// Saving over a desk scope forks it into the user layer at once and says
/// so on the status bar.
#[gpui::test]
fn saving_over_a_desk_scope_forks_without_asking_and_says_so(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.set_lane(book("BK002"));
    f.open_current();
    f.keys("s");
    f.type_text("desk_eu");
    f.keys("enter");
    assert!(!f.painted("scope-dialog-confirm"), "a fork does not ask");
    assert_eq!(f.top(), Some(Layer::Current));
    assert_eq!(
        f.notice().as_deref(),
        Some("copied 'desk_eu' to your config — r restores the desk copy")
    );
    assert_eq!(f.loaded_from().as_deref(), Some("desk_eu"));
    f.flush();
    let written = f.user_scopes();
    assert!(
        written.contains("[desk_eu") && written.contains("BK002"),
        "{written}"
    );
}

/// A scope saved a moment ago is already the user's: saving over it again
/// before the flush asks.
#[gpui::test]
fn saving_over_a_scope_just_created_asks(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.set_lane(book("BK002"));
    f.open_current();
    f.keys("s");
    f.type_text("mine");
    f.keys("enter");
    assert!(!f.painted("scope-dialog-confirm"));
    f.keys("s");
    assert_eq!(
        f.draft().as_deref(),
        Some("mine"),
        "seeded with the new source"
    );
    f.keys("enter");
    assert!(f.painted("scope-dialog-confirm"));
}

/// `n` drops the question and leaves the prompt as it was. While the
/// question is up it owns every key: `j` neither types nor acts.
#[gpui::test]
fn n_answers_no_and_keeps_the_draft(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.load("eu");
    f.open_current();
    f.keys("s");
    f.keys("enter");
    assert!(f.painted("scope-dialog-confirm"));
    f.keys("j");
    assert!(
        f.painted("scope-dialog-confirm"),
        "an unrecognised key is dropped"
    );
    assert_eq!(f.draft().as_deref(), Some("eu"), "it does not type");
    assert_eq!(f.input_text(), "eu");
    f.keys("n");
    assert!(!f.painted("scope-dialog-confirm"));
    assert_eq!(f.top(), save_step());
    assert_eq!(f.draft().as_deref(), Some("eu"));
    assert_eq!(f.input_text(), "eu");
    assert!(f.painted("scope-dialog-name-field"));
    assert!(!f.queued(), "nothing was queued");
}

/// An empty scope has nothing to save: `s` refuses into the dialog's error
/// line, and the one-shot door refuses on the status bar and opens nothing.
#[gpui::test]
fn saving_an_empty_scope_refuses(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.open_current();
    f.keys("s");
    assert_eq!(f.top(), Some(Layer::Current), "no prompt");
    assert!(!f.painted("scope-dialog-name-field"));
    assert!(f.painted("scope-dialog-error"));
    assert_eq!(
        error(&f.shell, &f.vcx).as_deref(),
        Some("nothing to save — the scope is empty")
    );
    f.keys("escape");
    assert_eq!(f.top_kind(), None);
    dispatch_action(&f.shell, "scope::save_current", &mut f.vcx);
    draw(&mut f.vcx);
    assert_eq!(f.top_kind(), None, "the one-shot door opens nothing");
    assert_eq!(
        f.notice().as_deref(),
        Some("nothing to save — the scope is empty")
    );
}

/// A reserved name and one the configuration cannot hold refuse under the
/// field, and the prompt stays.
#[gpui::test]
fn reserved_and_unusable_names_refuse_inline(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.set_lane(book("BK002"));
    f.open_current();
    f.keys("s");
    f.type_text("save_current");
    f.keys("enter");
    assert_eq!(f.top(), save_step());
    assert!(f.painted("scope-dialog-error"));
    assert_eq!(
        f.prompt_error().as_deref(),
        Some("'save_current' is reserved")
    );
    f.keys("escape");
    assert_eq!(f.top(), Some(Layer::Current));
    f.keys("s");
    f.type_text("a b");
    f.keys("enter");
    assert_eq!(f.top(), save_step());
    assert_eq!(
        f.prompt_error().as_deref(),
        Some("'a b' is not a usable name")
    );
    f.type_text("c");
    assert_eq!(f.prompt_error(), None, "typing clears the refusal");
    assert!(!f.queued(), "nothing was queued");
}

/// The save chip opens the prompt alone: `enter` saves and closes the
/// dialog.
#[gpui::test]
fn the_save_chip_opens_the_prompt_alone_and_enter_closes(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.set_lane(book("BK002"));
    let chip = f
        .vcx
        .debug_bounds("scope-save-chip")
        .expect("the save chip paints over a nonempty scope");
    f.vcx
        .simulate_click(chip.center(), gpui::Modifiers::default());
    f.vcx.run_until_parked();
    draw(&mut f.vcx);
    assert_eq!(f.top_kind(), Some(dialog::DialogKind::Scope));
    assert_eq!(f.top(), save_step());
    assert_eq!(depth(&f.shell, &f.vcx), 1, "the prompt is the bottom layer");
    assert!(f.painted("scope-dialog-name-field"));
    f.type_text("mine");
    f.keys("enter");
    assert_eq!(f.top_kind(), None);
    assert!(f.has_saved("mine"));
    assert_eq!(f.loaded_from().as_deref(), Some("mine"));
}

/// `scope::save_current` is the chip's keyboard door: the prompt alone,
/// and `escape` closes it with nothing written.
#[gpui::test]
fn scope_save_current_opens_the_prompt_alone_and_escape_closes(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.set_lane(book("BK002"));
    dispatch_action(&f.shell, "scope::save_current", &mut f.vcx);
    draw(&mut f.vcx);
    assert_eq!(f.top(), save_step());
    assert_eq!(depth(&f.shell, &f.vcx), 1);
    f.type_text("mine");
    f.keys("escape");
    assert_eq!(f.top_kind(), None);
    assert!(!f.has_saved("mine"));
    assert!(!f.queued());
}

/// Keys the focused field binds (backspace, delete) would edit the draft
/// behind the question: while it is up the field gives up focus, and `n`
/// hands it back with the draft as it was.
#[gpui::test]
fn a_question_keeps_editing_keys_off_the_draft(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.load("eu");
    f.open_current();
    f.keys("s");
    f.keys("enter");
    assert!(f.painted("scope-dialog-confirm"));
    let input_focused = |f: &mut SaveFixture| {
        let shell = f.shell.clone();
        f.vcx.update(|window, cx| {
            shell
                .read(cx)
                .dialog_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        })
    };
    f.keys("backspace");
    f.keys("left delete");
    // An unrecognised chord is claimed too: `alt-p` would push the picker
    // over the question.
    f.keys("alt-p");
    assert_eq!(f.top_kind(), Some(dialog::DialogKind::Scope));
    assert!(f.painted("scope-dialog-confirm"));
    assert_eq!(f.draft().as_deref(), Some("eu"));
    assert_eq!(f.input_text(), "eu");
    assert!(
        !input_focused(&mut f),
        "the question takes focus off the field"
    );
    f.keys("n");
    assert_eq!(f.top(), save_step());
    assert_eq!(f.draft().as_deref(), Some("eu"));
    assert_eq!(f.input_text(), "eu");
    assert!(input_focused(&mut f), "the field has focus back");
}

// ---- The definition step ----------------------------------------------

use crate::shell::scopedialog::state::Step;

fn definition_step(name: Option<&str>) -> Option<Layer> {
    Some(Layer::Step(Step::Definition {
        name: name.map(str::to_string),
    }))
}

impl SaveFixture {
    /// The Scope dialog on Current over a lane naming `liq`, then `o`.
    fn open_saved_over_liq(&mut self) {
        self.set_lane(liq_scope());
        self.open_current();
        self.keys("o");
        assert_eq!(
            visible_names(&self.shell, &self.vcx),
            ["desk_eu", "eu", "big", "liq"]
        );
    }

    fn definition_draft(&self) -> Option<String> {
        self.shell.read_with(&self.vcx, |s, _| {
            s.scope_dialog
                .as_ref()
                .and_then(|d| d.definition.as_ref().map(|d| d.draft.clone()))
        })
    }

    fn definition_note(&self) -> Option<String> {
        self.shell.read_with(&self.vcx, |s, _| {
            s.scope_dialog
                .as_ref()
                .and_then(|d| d.definition.as_ref())
                .and_then(|d| d.note.as_ref().map(|n| n.to_string()))
        })
    }

    fn definition_error(&self) -> Option<String> {
        self.shell.read_with(&self.vcx, |s, _| {
            s.scope_dialog
                .as_ref()
                .and_then(|d| d.definition.as_ref())
                .and_then(|d| d.error.clone())
        })
    }

    fn named_text(&self, name: &str) -> Option<String> {
        frame_of(&self.shell, &self.vcx).read_with(&self.vcx, |f, _| {
            f.named_expressions()
                .get(name)
                .map(|d| d.text().to_string())
        })
    }

    fn user_expressions(&self) -> String {
        std::fs::read_to_string(self.user.path().join("expressions.toml")).unwrap_or_default()
    }

    /// `enter` dispatched and the frame read in one update: no task runs
    /// between them, so only the step's own refresh can have resolved it.
    fn enter_and_read_named(&mut self, name: &str) -> Option<String> {
        let frame = frame_of(&self.shell, &self.vcx);
        let text = self.vcx.update(|window, cx| {
            window.dispatch_keystroke(gpui::Keystroke::parse("enter").unwrap(), cx);
            frame
                .read(cx)
                .named_expressions()
                .get(name)
                .map(|d| d.text().to_string())
        });
        self.vcx.run_until_parked();
        draw(&mut self.vcx);
        text
    }

    /// Open `liq`'s definition from Saved and change it to `npv > 5`.
    fn edit_liq_to_npv_gt_5(&mut self) -> Option<String> {
        self.open_saved_over_liq();
        self.keys("j j j e");
        assert_eq!(self.top(), definition_step(Some("liq")));
        self.keys("backspace");
        self.type_text("5");
        assert_eq!(self.definition_draft().as_deref(), Some("npv > 5"));
        self.enter_and_read_named("liq")
    }
}

/// `e` on an expression row opens its definition, seeded with its text and
/// a note saying who uses it; `enter` writes it, resolved in the frame at
/// once, and Saved shows again.
#[gpui::test]
fn e_on_an_expression_edits_its_definition_with_a_used_by_note(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.open_saved_over_liq();
    f.keys("j j j e");
    assert_eq!(f.top(), definition_step(Some("liq")));
    assert_eq!(f.top_kind(), Some(dialog::DialogKind::Scope));
    assert!(f.painted("scope-dialog-definition-field"));
    assert!(f.painted("scope-dialog-definition-note"));
    assert_eq!(
        f.definition_note().as_deref(),
        Some("Used by the current scope.")
    );
    assert_eq!(f.definition_draft().as_deref(), Some("npv > 0"));
    assert_eq!(f.input_text(), "npv > 0", "the field mirrors the draft");
    f.keys("backspace");
    f.type_text("5");
    assert_eq!(
        f.enter_and_read_named("liq").as_deref(),
        Some("npv > 5"),
        "resolved before the flush"
    );
    assert_eq!(f.top(), Some(Layer::Saved));
    assert!(
        f.notice()
            .is_some_and(|n| n.starts_with("copied 'liq' to your config")),
        "{:?}",
        f.notice()
    );
    f.flush();
    let written = f.user_expressions();
    assert!(
        written.contains("[liq") && written.contains("npv > 5"),
        "{written}"
    );
}

/// The lane still names `liq`, and its effective scope now reads the new
/// definition: references follow an edit in place.
#[gpui::test]
fn editing_a_definition_reaches_the_current_scope_at_once(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    let frame = frame_of(&f.shell, &f.vcx);
    let before = frame.read_with(&f.vcx, |fr, _| fr.shared().versions().config);
    assert_eq!(f.edit_liq_to_npv_gt_5().as_deref(), Some("npv > 5"));
    assert_eq!(lane_scope(&f.shell, &f.vcx), liq_scope());
    let (resolved, after) = frame.read_with(&f.vcx, |fr, _| {
        (
            fr.shared().effective_scope(&Scope::default()),
            fr.shared().versions().config,
        )
    });
    let resolved = resolved.expect("liq resolves");
    assert_eq!(
        resolved.expression.map(|e| e.to_string()).as_deref(),
        Some("npv > 5")
    );
    assert!(after > before, "the config version advanced");
}

/// The definition field suggests columns but never named rows: one
/// definition does not refer to another. An unused expression's note says
/// so.
#[gpui::test]
fn the_definition_field_offers_columns_but_no_named_rows(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.open_saved_over_liq();
    f.keys("j j e"); // big
    assert_eq!(f.top(), definition_step(Some("big")));
    assert_eq!(f.definition_note().as_deref(), Some("not used"));
    f.keys("escape");
    assert_eq!(f.top(), Some(Layer::Saved));
    f.keys("n");
    assert_eq!(f.top(), definition_step(None));
    assert_eq!(f.input_text(), "");
    assert!(
        !f.painted("scope-dialog-definition-note"),
        "a new one has no note"
    );
    f.type_text("n");
    assert!(f.painted("scope-expr-row-npv"), "a column is offered");
    assert!(!f.painted("scope-expr-named-row-liq"));
    assert!(!f.painted("scope-expr-named-row-big"));
    // `b` would match both `book` and the name `big`, were names offered.
    f.keys("backspace");
    f.type_text("b");
    assert!(f.painted("scope-expr-row-book"));
    assert!(!f.painted("scope-expr-named-row-big"), "no named rows");
}

/// `tab` accepts the highlighted column into the field, and the draft holds
/// what the field now reads: the input is not put back to the typed prefix.
#[gpui::test]
fn tab_accepts_a_column_into_the_definition_draft(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.open_saved_over_liq();
    f.keys("j j n"); // on big
    f.type_text("np");
    assert!(f.painted("scope-expr-row-npv"));
    f.keys("tab");
    let draft = f.definition_draft().unwrap_or_default();
    assert!(draft.starts_with("npv"), "{draft:?}");
    assert_eq!(f.input_text(), draft, "the field and the draft agree");
}

/// An empty, unparseable or unknown-column definition refuses under the
/// field, which stays open; nothing is written.
#[gpui::test]
fn an_empty_or_broken_definition_refuses_inline(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.open_saved_over_liq();
    f.keys("j j j e");
    f.keys("backspace backspace backspace backspace backspace backspace backspace");
    assert_eq!(f.definition_draft().as_deref(), Some(""));
    f.keys("enter");
    assert_eq!(
        f.definition_error().as_deref(),
        Some("an empty named expression would match everything")
    );
    assert!(f.painted("scope-dialog-error"));
    assert_eq!(f.top(), definition_step(Some("liq")));
    f.type_text("npv >");
    assert_eq!(f.definition_error(), None, "typing clears the refusal");
    f.keys("enter");
    assert!(f.definition_error().is_some(), "a parse error refuses");
    assert_eq!(f.top(), definition_step(Some("liq")));
    f.keys("backspace backspace backspace backspace backspace");
    f.type_text("nvp > 1");
    f.keys("enter");
    assert!(
        f.definition_error()
            .is_some_and(|e| e.contains("unknown column 'nvp'")),
        "{:?}",
        f.definition_error()
    );
    assert_eq!(f.top(), definition_step(Some("liq")));
    assert_eq!(f.named_text("liq").as_deref(), Some("npv > 0"));
    assert!(!f.queued(), "nothing queued");
}

/// `n` opens an empty definition; `enter` turns it into a name prompt in
/// place; naming it writes it without touching the lane's scope, and Saved
/// shows again.
#[gpui::test]
fn n_names_a_new_expression_without_applying_it(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.open_saved_over_liq();
    f.keys("j j n");
    assert_eq!(f.top(), definition_step(None));
    f.type_text("npv < 1");
    f.keys("enter");
    assert_eq!(
        f.top(),
        Some(Layer::Step(Step::NameExpression {
            text: "npv < 1".into()
        }))
    );
    assert_eq!(depth(&f.shell, &f.vcx), 3, "replaces the step in place");
    assert!(f.painted("scope-dialog-name-field"));
    assert_eq!(f.input_text(), "", "the field is the name now");
    f.type_text("small");
    assert_eq!(f.enter_and_read_named("small").as_deref(), Some("npv < 1"));
    assert_eq!(f.top(), Some(Layer::Saved));
    assert_eq!(lane_scope(&f.shell, &f.vcx), liq_scope(), "not applied");
    assert_eq!(f.notice(), None, "a new name forks nothing");
    f.flush();
    let written = f.user_expressions();
    assert!(
        written.contains("[small") && written.contains("npv < 1"),
        "{written}"
    );
}

/// Naming a new expression paints the prompt alone with the text being
/// named under it: not the lane's scope rows, which it does not join.
#[gpui::test]
fn naming_a_new_expression_previews_its_text_not_the_scope(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.open_saved_over_liq();
    f.keys("j j n");
    f.type_text("npv < 1");
    f.keys("enter");
    assert!(f.painted("scope-dialog-name-field"));
    assert!(f.painted("scope-dialog-name-preview"));
    assert!(!f.painted("scope-dialog"), "Current's rows do not paint");
    assert!(!f.painted("scope-dialog-row-0"));
    // The save prompt still previews the lane's scope: that is what it saves.
    f.keys("escape escape");
    assert_eq!(f.top(), Some(Layer::Current));
    f.keys("s");
    assert!(f.painted("scope-dialog-row-0"));
    assert!(!f.painted("scope-dialog-name-preview"));
}

/// A name already defined at any layer, or reserved, refuses under the
/// field with nothing written.
#[gpui::test]
fn naming_a_new_expression_refuses_a_taken_name(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.open_saved_over_liq();
    f.keys("j j n"); // on big
    f.type_text("npv < 1");
    f.keys("enter");
    f.type_text("big");
    f.keys("enter");
    assert_eq!(f.prompt_error().as_deref(), Some("'big' already exists"));
    assert!(f.painted("scope-dialog-error"));
    assert!(matches!(
        f.top(),
        Some(Layer::Step(Step::NameExpression { .. }))
    ));
    f.keys("backspace backspace backspace");
    f.type_text("save_current");
    f.keys("enter");
    assert_eq!(
        f.prompt_error().as_deref(),
        Some("'save_current' is reserved")
    );
    assert!(!f.queued(), "nothing queued");
    assert_eq!(f.named_text("big").as_deref(), Some("npv > 100"));
}

/// The `≡` chip's body opens the definition alone: its commit closes the
/// dialog.
#[gpui::test]
fn the_named_chip_opens_its_definition_alone(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.set_lane(liq_scope());
    let chip = f
        .vcx
        .debug_bounds("scope-named-chip-liq")
        .expect("the chip paints");
    f.vcx
        .simulate_click(chip.center(), gpui::Modifiers::default());
    f.vcx.run_until_parked();
    draw(&mut f.vcx);
    assert_eq!(f.top_kind(), Some(dialog::DialogKind::Scope));
    assert_eq!(f.top(), definition_step(Some("liq")));
    assert_eq!(depth(&f.shell, &f.vcx), 1, "the step is the bottom layer");
    assert_eq!(f.input_text(), "npv > 0");
    f.keys("backspace");
    f.type_text("5");
    f.keys("enter");
    assert_eq!(f.top_kind(), None, "the commit closes the dialog");
    assert_eq!(f.named_text("liq").as_deref(), Some("npv > 5"));
}

/// Saved opened alone over a frame with `scopes` and no saved expressions.
fn saved_without_expressions(
    cx: &mut gpui::TestAppContext,
    keep_scopes: bool,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    let (shell, mut vcx) = shell_on_liq(cx);
    frame_of(&shell, &vcx).update(&mut vcx, |f, cx| {
        if !keep_scopes {
            f.replace_saved_scopes(SavedScopes::new());
        }
        f.replace_named_expressions(geode_core::named::NamedExpressions::default());
        cx.notify();
    });
    vcx.run_until_parked();
    dispatch_action(&shell, "frame::scope_saved", &mut vcx);
    draw(&mut vcx);
    (shell, vcx)
}

/// With saved scopes but no expressions, the empty Expressions row is a
/// cursor stop: `j` reaches it, the footer says what `enter` does there,
/// and `n` and `enter` both start a new expression. The cursor stays on
/// the row across the step's visit.
#[gpui::test]
fn the_empty_expressions_row_is_a_cursor_stop_for_n_and_enter(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = saved_without_expressions(cx, true);
    assert_eq!(visible_names(&shell, &vcx), ["asia", "eu"]);
    assert!(vcx.debug_bounds("scope-saved-hint-new").is_none());
    vcx.simulate_keystrokes("j j");
    draw(&mut vcx);
    assert!(
        vcx.debug_bounds("scope-saved-hint-new").is_some(),
        "the footer offers a new expression"
    );
    vcx.simulate_keystrokes("n");
    draw(&mut vcx);
    assert_eq!(top_layer(&shell, &vcx), definition_step(None));
    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);
    assert_eq!(top_layer(&shell, &vcx), Some(Layer::Saved));
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert_eq!(
        top_layer(&shell, &vcx),
        definition_step(None),
        "the cursor stayed on the empty row"
    );
}

/// The empty Scopes row is a cursor stop too; `enter` and `n` there give
/// the scope rows' guidance.
#[gpui::test]
fn the_empty_scopes_row_refuses_with_the_way_to_save_one(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = saved_without_expressions(cx, false);
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert_eq!(
        error(&shell, &vcx).as_deref(),
        Some("narrow the current scope, then save it (s)")
    );
    vcx.simulate_keystrokes("n");
    draw(&mut vcx);
    assert_eq!(
        error(&shell, &vcx).as_deref(),
        Some("narrow the current scope, then save it (s)")
    );
    assert_eq!(top_layer(&shell, &vcx), Some(Layer::Saved));
    vcx.simulate_keystrokes("j n");
    draw(&mut vcx);
    assert_eq!(top_layer(&shell, &vcx), definition_step(None));
}

/// The Expressions section's empty row is also the pointer route of `n`.
#[gpui::test]
fn n_and_the_empty_expressions_row_open_a_new_definition(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = saved_without_expressions(cx, false);
    let empty = vcx
        .debug_bounds("scope-saved-empty-expressions")
        .expect("the empty row paints");
    vcx.simulate_click(empty.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    draw(&mut vcx);
    assert_eq!(top_layer(&shell, &vcx), definition_step(None));
}

/// `e` on an expression removed since the rows derived refuses and opens
/// nothing.
#[gpui::test]
fn e_on_a_vanished_expression_says_so(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_saved_from_current(cx);
    vcx.simulate_keystrokes("j j"); // big
    frame_of(&shell, &vcx).update(&mut vcx, |f, _| {
        let named = geode_core::named::NamedExpressions::default();
        assert!(f.replace_named_expressions(named));
    });
    vcx.simulate_keystrokes("e");
    draw(&mut vcx);
    assert_eq!(
        error(&shell, &vcx).as_deref(),
        Some("that expression no longer exists")
    );
    assert_eq!(top_layer(&shell, &vcx), Some(Layer::Saved));
}

// ---- Copy, delete and revert ------------------------------------------

use crate::shell::scopedialog::saved::SavedId;

impl SaveFixture {
    fn copy_label_and_preview(&self) -> Option<(String, Option<String>)> {
        self.shell.read_with(&self.vcx, |s, _| {
            let prompt = s.scope_dialog.as_ref()?.prompt.as_ref()?;
            Some((
                prompt.purpose.label().to_string(),
                prompt.purpose.preview().map(|p| p.to_string()),
            ))
        })
    }

    /// The question up: its text, its detail line and its yes label.
    fn question(&self) -> Option<(String, Option<String>, &'static str)> {
        self.shell.read_with(&self.vcx, |s, _| {
            let pending = s.scope_dialog.as_ref()?.pending.as_ref()?;
            Some((
                pending.question.clone(),
                pending.detail.clone(),
                pending.yes_label,
            ))
        })
    }

    fn saved_cursor(&self) -> Option<SavedId> {
        self.shell.read_with(&self.vcx, |s, _| {
            s.scope_dialog
                .as_ref()?
                .saved
                .cursor_row()
                .map(|r| r.id.clone())
        })
    }

    fn saved_summary(&self, name: &str) -> Option<String> {
        frame_of(&self.shell, &self.vcx).read_with(&self.vcx, |f, _| {
            f.saved_scopes()
                .get(name)
                .map(crate::shell::scopedialog::saved::summary)
        })
    }

    /// `key` dispatched and the frame's saved scopes read in one update:
    /// no task runs between them, so the batch has not flushed and only the
    /// handler's own refresh can have resolved the change.
    fn key_and_read_saved(&mut self, key: &str, name: &str) -> bool {
        let frame = frame_of(&self.shell, &self.vcx);
        let held = self.vcx.update(|window, cx| {
            window.dispatch_keystroke(gpui::Keystroke::parse(key).unwrap(), cx);
            frame.read(cx).saved_scopes().contains_key(name)
        });
        self.vcx.run_until_parked();
        draw(&mut self.vcx);
        held
    }

    fn saved_error(&self) -> Option<String> {
        error(&self.shell, &self.vcx)
    }

    /// Fork `desk_eu` into the user layer with book BK002 (the desk copy
    /// holds BK003), flush it, and open Saved over Current on `desk_eu`.
    fn fork_desk_eu_and_open_saved(&mut self) {
        self.set_lane(book("BK002"));
        self.open_current();
        self.keys("s");
        self.type_text("desk_eu");
        self.keys("enter");
        self.flush();
        assert!(self.user_scopes().contains("[desk_eu"));
        self.keys("o");
        assert_eq!(top_layer(&self.shell, &self.vcx), Some(Layer::Saved));
        assert_eq!(self.saved_cursor(), Some(SavedId::Scope("desk_eu".into())));
    }

    /// Reload the configuration as it is, less `name` in `layer`'s `doc`:
    /// a change made elsewhere while the dialog is up.
    fn reload_without(&mut self, layer: geode_core::config::Layer, doc: &str, name: &str) {
        let (doc, name) = (doc.to_string(), name.to_string());
        self.shell.update(&mut self.vcx, |s, cx| {
            let docs = s
                .services
                .config
                .all_docs()
                .into_iter()
                .map(|mut d| {
                    if d.layer == layer && d.name == doc {
                        d.table.remove(&name);
                    }
                    d
                })
                .collect();
            s.apply_reload(geode_core::config::Config::from_docs(docs), cx);
        });
        self.vcx.run_until_parked();
        draw(&mut self.vcx);
    }
}

fn copy_step(from: SavedId) -> Option<Layer> {
    Some(Layer::Step(Step::CopyName { from }))
}

/// `c` names a copy of the scope under the cursor: the prompt says what it
/// copies and previews its summary; a taken name refuses; `enter` writes
/// the source's definition under the new name and Saved shows again with
/// the cursor on the copy.
#[gpui::test]
fn c_copies_a_scope_under_a_new_name(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.open_saved_over_liq();
    f.keys("j c"); // eu
    assert_eq!(f.top(), copy_step(SavedId::Scope("eu".into())));
    assert!(f.painted("scope-dialog-name-field"));
    assert!(f.painted("scope-dialog-name-preview"));
    assert!(!f.painted("scope-dialog"), "Current's rows do not paint");
    assert_eq!(
        f.copy_label_and_preview(),
        Some(("Copy 'eu' as".into(), Some("book BK001".into())))
    );
    assert_eq!(f.input_text(), "", "the name starts empty");
    f.type_text("desk_eu");
    f.keys("enter");
    assert_eq!(
        f.prompt_error().as_deref(),
        Some("'desk_eu' already exists")
    );
    assert!(!f.queued(), "a taken name writes nothing");
    f.keys("backspace backspace backspace backspace backspace backspace backspace");
    f.type_text("eu2");
    assert!(
        f.key_and_read_saved("enter", "eu2"),
        "resolved before the flush"
    );
    assert_eq!(f.top(), Some(Layer::Saved));
    assert_eq!(f.saved_summary("eu2").as_deref(), Some("book BK001"));
    assert_eq!(f.saved_cursor(), Some(SavedId::Scope("eu2".into())));
    assert_eq!(f.notice(), None, "a new name forks nothing");
    assert_eq!(lane_scope(&f.shell, &f.vcx), liq_scope(), "nothing loaded");
    f.flush();
    let written = f.user_scopes();
    assert!(
        written.contains("[eu2") && written.contains("[eu]"),
        "{written}"
    );
}

/// `c` on an expression copies its text, previewed in the prompt.
#[gpui::test]
fn c_copies_an_expression_with_its_text(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.open_saved_over_liq();
    f.keys("j j j c"); // liq
    assert_eq!(f.top(), copy_step(SavedId::Expression("liq".into())));
    assert_eq!(
        f.copy_label_and_preview(),
        Some(("Copy 'liq' as".into(), Some("npv > 0".into())))
    );
    f.type_text("liq2");
    f.keys("enter");
    assert_eq!(f.top(), Some(Layer::Saved));
    assert_eq!(f.named_text("liq2").as_deref(), Some("npv > 0"));
    assert_eq!(f.saved_cursor(), Some(SavedId::Expression("liq2".into())));
    assert_eq!(lane_scope(&f.shell, &f.vcx), liq_scope(), "not applied");
    f.flush();
    let written = f.user_expressions();
    assert!(
        written.contains("[liq2") && written.contains("npv > 0"),
        "{written}"
    );
}

/// `d` on a user scope asks; `y` removes it from the user layer and the
/// cursor lands on the next row.
#[gpui::test]
fn d_deletes_a_user_scope_after_yes(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.open_saved_over_liq();
    f.keys("j d"); // eu
    assert!(f.painted("scope-dialog-confirm"));
    assert_eq!(
        f.question(),
        Some(("Delete 'eu' from your config?".into(), None, "Delete"))
    );
    assert!(!f.queued(), "nothing queued before the answer");
    assert!(!f.key_and_read_saved("y", "eu"), "removed before the flush");
    assert!(!f.painted("scope-dialog-confirm"));
    assert_eq!(f.top(), Some(Layer::Saved));
    assert_eq!(visible_names(&f.shell, &f.vcx), ["desk_eu", "big", "liq"]);
    assert_eq!(f.saved_cursor(), Some(SavedId::Expression("big".into())));
    f.flush();
    assert!(!f.user_scopes().contains("[eu"), "{}", f.user_scopes());
}

/// Deleting an expression the lane names says so under the question; after
/// `y` the lane's reference is left broken, not silently dropped.
#[gpui::test]
fn deleting_a_used_expression_leaves_a_broken_reference(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture_with(cx, "config_version = 1\n[mine]\nexpression = \"npv > 5\"\n");
    f.set_lane(Scope {
        named: vec!["mine".into()],
        ..Scope::default()
    });
    f.open_current();
    f.keys("o");
    assert_eq!(
        visible_names(&f.shell, &f.vcx),
        ["desk_eu", "eu", "big", "liq", "mine"]
    );
    f.keys("j j j j d");
    assert!(f.painted("scope-dialog-confirm-detail"));
    assert_eq!(
        f.question(),
        Some((
            "Delete 'mine' from your config?".into(),
            Some("Used by the current scope.".into()),
            "Delete"
        ))
    );
    f.keys("y");
    assert_eq!(f.named_text("mine"), None);
    f.keys("escape");
    assert_eq!(f.top(), Some(Layer::Current));
    assert_eq!(
        lane_scope(&f.shell, &f.vcx).named,
        ["mine"],
        "the reference stays"
    );
    let broken = f.shell.read_with(&f.vcx, |s, _| {
        s.scope_dialog
            .as_ref()
            .and_then(|d| d.display.first().map(|r| (r.label.to_string(), r.broken)))
    });
    assert_eq!(broken, Some(("mine".into(), true)));
    f.flush();
    assert!(!f.user_expressions().contains("[mine"));
}

/// A desk or builtin definition has nothing of the user's to delete: `d`
/// refuses, naming the layer, and asks nothing.
#[gpui::test]
fn d_on_a_desk_scope_refuses_with_its_layer(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.open_saved_over_liq();
    f.keys("d"); // desk_eu
    assert!(!f.painted("scope-dialog-confirm"));
    assert_eq!(
        f.saved_error().as_deref(),
        Some("'desk_eu' comes from the desk layer — there is nothing of yours to delete")
    );
    assert!(f.painted("scope-dialog-error"));
    f.keys("j j d"); // big
    assert_eq!(
        f.saved_error().as_deref(),
        Some("'big' comes from the builtin layer — there is nothing of yours to delete")
    );
    assert!(!f.queued());
}

/// `r` on a forked scope asks; `y` removes the user copy and the desk copy
/// shows again, the cursor still on it.
#[gpui::test]
fn r_reverts_a_forked_scope_to_the_desk_copy(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.fork_desk_eu_and_open_saved();
    assert_eq!(f.saved_summary("desk_eu").as_deref(), Some("book BK002"));
    f.keys("r");
    assert_eq!(
        f.question(),
        Some((
            "Throw away your changes to 'desk_eu'?".into(),
            None,
            "Revert"
        ))
    );
    f.keys("y");
    assert!(!f.painted("scope-dialog-confirm"));
    assert_eq!(f.saved_summary("desk_eu").as_deref(), Some("book BK003"));
    assert_eq!(f.saved_cursor(), Some(SavedId::Scope("desk_eu".into())));
    f.flush();
    assert!(!f.user_scopes().contains("[desk_eu"), "{}", f.user_scopes());
}

/// A definition the user holds with nothing beneath it, or does not hold
/// at all, has no changes to revert.
#[gpui::test]
fn r_without_changes_refuses(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.open_saved_over_liq();
    f.keys("j r"); // eu: the user's own, nothing beneath
    assert!(!f.painted("scope-dialog-confirm"));
    assert_eq!(
        f.saved_error().as_deref(),
        Some("'eu' has no changes of yours to revert")
    );
    f.keys("k r"); // desk_eu: not the user's
    assert_eq!(
        f.saved_error().as_deref(),
        Some("'desk_eu' has no changes of yours to revert")
    );
    assert!(!f.queued());
}

/// The answer applies to the definition the question was asked about, as
/// it was: a revert whose lower copy vanished would now delete the user's
/// only copy, and a delete whose definition vanished has nothing to do.
/// Both refuse and remove nothing.
#[gpui::test]
fn a_question_answered_after_the_row_vanished_removes_nothing(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.fork_desk_eu_and_open_saved();
    f.keys("r");
    assert!(f.question().is_some());
    f.reload_without(geode_core::config::Layer::Desk, "scopes", "desk_eu");
    f.keys("y");
    assert_eq!(
        f.saved_error().as_deref(),
        Some("the list changed under the question — nothing was removed")
    );
    assert!(!f.queued(), "nothing queued");
    assert!(f.has_saved("desk_eu"), "the user's copy stays");
    assert!(f.user_scopes().contains("[desk_eu"));

    f.keys("j d"); // eu
    assert!(f.question().is_some());
    f.reload_without(geode_core::config::Layer::User, "scopes", "eu");
    f.keys("y");
    assert_eq!(
        f.saved_error().as_deref(),
        Some("the list changed under the question — nothing was removed")
    );
    assert!(!f.queued());
}

/// A question over Saved owns the pointer as well as the keys: a row's
/// double-click, the Back button and the filter row do nothing.
#[gpui::test]
fn a_question_over_saved_ignores_the_pointer(cx: &mut gpui::TestAppContext) {
    let mut f = save_fixture(cx);
    f.open_saved_over_liq();
    let row = f.vcx.debug_bounds("scope-saved-row-1").expect("eu paints");
    let back = f
        .vcx
        .debug_bounds("shell-modal-back")
        .expect("Saved over Current paints Back");
    let filter = f
        .vcx
        .debug_bounds("scope-saved-filter")
        .expect("the filter row paints");
    f.keys("j d");
    assert!(f.painted("scope-dialog-confirm"));
    super::double_click(&mut f.vcx, row.center(), gpui::Modifiers::default());
    f.vcx.run_until_parked();
    draw(&mut f.vcx);
    f.vcx
        .simulate_click(back.center(), gpui::Modifiers::default());
    f.vcx.run_until_parked();
    draw(&mut f.vcx);
    f.vcx
        .simulate_click(filter.center(), gpui::Modifiers::default());
    f.vcx.run_until_parked();
    draw(&mut f.vcx);
    assert_eq!(f.top(), Some(Layer::Saved));
    assert!(
        f.painted("scope-dialog-confirm"),
        "the question is still up"
    );
    assert_eq!(lane_scope(&f.shell, &f.vcx), liq_scope(), "nothing loaded");
    assert!(f.has_saved("eu"), "nothing removed");
    assert!(!f.queued());
    let mode = f
        .shell
        .read_with(&f.vcx, |s, _| s.scope_dialog.as_ref().map(|d| d.saved.mode));
    assert_eq!(mode, Some(crate::dialogmode::DialogMode::Normal));
}
