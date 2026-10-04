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
/// `desk_eu` (book BK003), over `services`' `risk` dataset, with a
/// writable user directory the saves land in.
struct SaveFixture {
    _desk: tempfile::TempDir,
    user: tempfile::TempDir,
    shell: Entity<ShellView>,
    vcx: gpui::VisualTestContext,
}

fn save_fixture(cx: &mut gpui::TestAppContext) -> SaveFixture {
    let desk = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
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
    f.keys("enter");
    assert!(!f.painted("scope-dialog-confirm"), "a new name never asks");
    assert!(f.has_saved("mine"), "resolved before the flush");
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
