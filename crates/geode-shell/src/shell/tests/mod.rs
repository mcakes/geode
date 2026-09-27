//! Shared fixtures for `ShellView` integration tests. Submodules cover individual shell
//! behaviors; helpers used across test files live here with `pub(super)` visibility.

use super::*;
use crate::actions::ActionId;
use crate::defaults::{
    BUILTIN_KEYMAP, default_mod, register_builtin_actions, register_pick_actions,
    register_scope_actions,
};
use crate::keymap::build_keymap;
use crate::tiling::{DockSide, Rect};
use geode_core::config::{ConfigSources, Layer, LayerDoc};
use gpui::{MouseButton, MouseDownEvent, MouseUpEvent, div, px};
use gpui_component::{Root, TITLE_BAR_HEIGHT};
// `use super::*` imports `WindowExt` and `Focusable` from `shell/mod.rs`. Dialog guards
// and the focus helpers below use those traits.

/// Fixture bindings layered over `BUILTIN_KEYMAP`: `ctrl+v` and `ctrl+h` add recorder
/// tiles horizontally and vertically. They use the same kind-specific actions a desk
/// keymap can bind; these chords are absent from the builtin keymap.
pub(super) const TEST_ADD_KEYMAP: &str = "[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"ctrl+v\" = \"tile::add_rec_horizontal\"\n\"ctrl+h\" = \"tile::add_rec_vertical\"\n";

/// `BUILTIN_KEYMAP` + [`TEST_ADD_KEYMAP`] + `extra`, built clean.
pub(super) fn test_keymap(registry: &ActionRegistry, extra: &[LayerDoc]) -> crate::keymap::Keymap {
    test_keymap_with_fragments(registry, &[], extra)
}

/// Build the test keymap with module fragments between compiled-in docs and editable
/// config layers, matching application startup order. Fixtures without fragments
/// exercise the empty-fragment case.
pub(super) fn test_keymap_with_fragments(
    registry: &ActionRegistry,
    fragments: &[LayerDoc],
    extra: &[LayerDoc],
) -> crate::keymap::Keymap {
    let mut docs = vec![
        LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
        LayerDoc::builtin("keymap", TEST_ADD_KEYMAP).unwrap(),
    ];
    docs.extend(extra.iter().cloned());
    let spliced = crate::keymap::fragments::splice(&docs, fragments);
    let (keymap, diags) = build_keymap(&spliced, default_mod(), registry);
    assert!(diags.is_empty(), "{diags:?}");
    keymap
}

pub(super) fn test_services() -> ShellServices {
    test_services_with_log().0
}

/// [`test_services`] plus the recorder's log — for a test that wants to
/// see what the "rec" occupant [`TEST_ADD_KEYMAP`]'s keys add was told.
pub(super) fn test_services_with_log() -> (
    ShellServices,
    std::rc::Rc<std::cell::RefCell<Vec<crate::module::recording::Recorded>>>,
) {
    let (services, log, _focus) = services_with_rec_roster();
    (services, log)
}

/// The shared roster registers the "rec" factory used by the fixture add bindings.
/// Adding to a placeholder fills it in place, so a real factory is necessary before a
/// second add can create another tile. There is no default kind: bare splits and
/// unknown restored kinds remain placeholders.
fn services_with_rec_roster() -> (
    ShellServices,
    std::rc::Rc<std::cell::RefCell<Vec<crate::module::recording::Recorded>>>,
    RecFocus,
) {
    let (services, log, focus, _input) = services_with_rec_roster_shipping(None);
    (services, log, focus)
}

/// An optional recorder fragment binds `q` in the module's `rec` context. Neither the
/// shell nor the fixture add layer binds that key, letting tests prove a module's own
/// binding reaches its occupant.
pub(super) const REC_FRAGMENT: &str =
    "[[bindings]]\ncontext = \"rec\"\n[bindings.keys]\n\"q\" = \"rec::noop\"\n";

/// [`services_with_rec_roster`] whose recorder ships `fragment` as its
/// [`crate::module::ModuleFactory::default_keymap`], the keymap built
/// over `splice` exactly as `main.rs` builds it — so the fragment reaches
/// the live keymap through the roster and nothing else.
pub(super) fn services_with_a_module_fragment(
    fragment: &'static str,
) -> (
    ShellServices,
    std::rc::Rc<std::cell::RefCell<Vec<crate::module::recording::Recorded>>>,
) {
    let (services, log, _focus, _input) = services_with_rec_roster_shipping(Some(fragment));
    assert!(
        services.keymap_fragment_diagnostics.is_empty(),
        "{:?}",
        services.keymap_fragment_diagnostics
    );
    (services, log)
}

/// Recorder insert-mode bindings use separate context tables for normal and insert
/// modes. `i` opens an input; `j` moves only in normal mode; Escape and Enter remain
/// module commands while typing.
pub(super) const REC_INSERT_FRAGMENT: &str = "\
[[bindings]]
context = \"rec && mode == normal\"
[bindings.keys]
\"i\" = \"rec::edit\"
\"j\" = \"rec::down\"

[[bindings]]
context = \"rec && mode == insert\"
[bindings.keys]
\"escape\" = \"rec::cancel\"
\"enter\" = \"rec::commit\"
";

/// The cell a `RecordingFactory` publishes its hosted view's live
/// `InputState` into — see that field's doc comment for why a test can
/// reach it no other way, and why nothing outside it may hold a clone.
pub(super) type RecInput = std::rc::Rc<std::cell::RefCell<Option<Entity<InputState>>>>;

/// [`services_with_a_module_fragment`] plus the recorder's input cell —
/// for the insert-mode tests, which have to read what typing landed in
/// the tile's own `Input`.
pub(super) fn services_with_an_insert_recorder(
    fragment: &'static str,
) -> (
    ShellServices,
    std::rc::Rc<std::cell::RefCell<Vec<crate::module::recording::Recorded>>>,
    RecInput,
) {
    let (services, log, _focus, input) = services_with_rec_roster_shipping(Some(fragment));
    assert!(
        services.keymap_fragment_diagnostics.is_empty(),
        "{:?}",
        services.keymap_fragment_diagnostics
    );
    (services, log, input)
}

/// The value in the recorder's own `Input`, or `None` when it has none.
/// Takes and drops a temporary clone of the entity deliberately: a clone
/// kept by a test would keep the `FocusHandle` alive and disprove the
/// dropped-focus net it is meant to observe (`RecordingFactory::input`).
pub(super) fn rec_input_value(input: &RecInput, cx: &gpui::VisualTestContext) -> Option<String> {
    let state = input.borrow().clone();
    state.map(|state| state.read_with(cx, |state, _| state.value().to_string()))
}

/// A fragment one of whose bindings `check_fragment` DROPS — it names
/// `workspace`, which the recorder does not declare. The fixture for
/// "does that diagnostic survive a hot reload": by then the offending
/// binding is long gone from the doc, so nothing but
/// `ShellServices::keymap_fragment_diagnostics` still knows about it.
pub(super) const REC_FRAGMENT_WITH_A_FOREIGN_BINDING: &str = "[[bindings]]\ncontext = \"rec\"\n[bindings.keys]\n\"q\" = \"rec::noop\"\n\n[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"ctrl+q\" = \"rec::noop\"\n";

/// [`services_with_a_module_fragment`]'s twin for that fragment: the one
/// error diagnostic is asserted here, so a test can be about what happens
/// to it rather than about whether it was produced.
pub(super) fn services_with_a_dropped_fragment_binding() -> ShellServices {
    let (services, _log, _focus, _input) =
        services_with_rec_roster_shipping(Some(REC_FRAGMENT_WITH_A_FOREIGN_BINDING));
    assert_eq!(
        services.keymap_fragment_diagnostics.len(),
        1,
        "{:?}",
        services.keymap_fragment_diagnostics
    );
    assert_eq!(
        services.keymap_fragment_diagnostics[0].severity,
        geode_core::config::Severity::Error
    );
    services
}

/// A `ShellServices` whose roster is exactly `recorders`, in order, built
/// in `main.rs`'s startup order (builtin actions, pick and scope actions,
/// add actions for every recorder kind, module actions, fragments, keymap).
pub(super) fn services_with_recorders(
    recorders: Vec<crate::module::recording::RecordingFactory>,
) -> ShellServices {
    use crate::module::ModuleFactory as _;
    // No compiled-in builtin layer in this fixture, so a reload has
    // nothing to preserve. `ShellServices::config_and_builtin` is the
    // constructor that keeps `config` and `builtin` from disagreeing
    // (its doc comment has the full rationale) and every other fixture
    // in this crate builds the pair through it — but that does NOT mean
    // every `ShellServices` here mirrors the two: `reload.rs`'s
    // `config_with_theme` call sites assign `services.config` directly,
    // mid-test, to drive `apply_reload` alone, which never reads
    // `builtin` — mirroring there would be inert, not wrong. Anyone
    // adding a reload (`reload::load_config`) assertion at one of those
    // sites must build a real pair first, or it would silently model a
    // shell whose reload deletes its own config.
    let (config, builtin) = ShellServices::config_and_builtin(ConfigSources::default());
    let mut registry = ActionRegistry::default();
    register_builtin_actions(&mut registry);
    // The startup ordering `main.rs` uses (`register_pick_actions` right
    // after `register_builtin_actions`, before `build_keymap`), exercised
    // here even though this config has no `datasets` doc — `pickable_
    // columns` then returns empty and the loop is a no-op, but the path
    // itself still runs on every test built from this fixture.
    register_pick_actions(&mut registry, &crate::shell::pickable_columns(&config));
    // Register saved-scope actions in startup order. This config has no scopes, so the
    // loop is empty while still exercising the common construction path.
    register_scope_actions(&mut registry, &crate::shell::saved_scopes(&config, false));
    // Register recorder add actions from the roster before building the keymap,
    // matching application startup.
    let kinds: Vec<&'static str> = recorders.iter().map(|r| r.kind()).collect();
    crate::defaults::register_add_actions(&mut registry, &kinds);
    let mut roster = crate::module::ModuleRoster::new();
    for r in recorders {
        roster.add(Box::new(r));
    }
    // Module actions exist before `build_keymap`, exactly as `main.rs`
    // orders it — a binding into the module's own context is what
    // `services_with_recorder`'s extra layer needs to resolve.
    roster.register_actions(&mut registry);
    // Collect fragments from the finished roster. Callers assert their diagnostics
    // because one fixture intentionally includes a binding that must be dropped.
    let (keymap_fragments, keymap_fragment_diagnostics) = roster.keymap_fragments();
    let mod_alias = default_mod();
    let keymap = test_keymap_with_fragments(&registry, &keymap_fragments, &[]);
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
        roster,
        restored_tiles: crate::session::TileRecords::new(),
        restored_frame: None,
        restored_palette_usage: crate::palette_usage::PaletteUsage::new(),
        log: None,
        action_tail: std::sync::Arc::new(std::sync::Mutex::new(
            crate::diagnostics::ActionTail::new(),
        )),
        keymap_diagnostics: Vec::new(),
        keymap_fragments,
        keymap_fragment_diagnostics,
    }
}

fn services_with_rec_roster_shipping(
    fragment: Option<&'static str>,
) -> (
    ShellServices,
    std::rc::Rc<std::cell::RefCell<Vec<crate::module::recording::Recorded>>>,
    RecFocus,
    RecInput,
) {
    let mut recorder = crate::module::recording::RecordingFactory::new("rec");
    recorder.fragment = fragment;
    let log = recorder.log.clone();
    let last_focus = recorder.last_focus.clone();
    let input = recorder.input.clone();
    (
        services_with_recorders(vec![recorder]),
        log,
        last_focus,
        input,
    )
}

/// The shared services with a binding into the recording module's own context, used to
/// verify occupant dispatch. The roster contains "rec" and has no default kind.
pub(super) fn services_with_recorder() -> (
    ShellServices,
    std::rc::Rc<std::cell::RefCell<Vec<crate::module::recording::Recorded>>>,
) {
    let (services, log, _focus) = services_with_recorder_focus_inner();
    (services, log)
}

/// The cell a `RecordingFactory` publishes its most recent view's
/// `FocusHandle` into — see that field's doc comment for why a test
/// cannot reach the handle any other way.
pub(super) type RecFocus = std::rc::Rc<std::cell::RefCell<Option<gpui::FocusHandle>>>;

/// [`services_with_recorder`], plus the recorder's focus cell — for the
/// one test that must put keyboard focus inside a tile WITHOUT a
/// mouse-down (a mouse-down re-arms `pending_focus_restore`, which is
/// the very thing that test must not have happen).
pub(super) fn services_with_recorder_focus() -> (ShellServices, RecFocus) {
    let (services, _log, focus) = services_with_recorder_focus_inner();
    (services, focus)
}

fn services_with_recorder_focus_inner() -> (
    ShellServices,
    std::rc::Rc<std::cell::RefCell<Vec<crate::module::recording::Recorded>>>,
    RecFocus,
) {
    let (mut services, log, focus) = services_with_rec_roster();
    // A binding into the module's own key context, so a key can be seen
    // to reach it.
    let module_doc = LayerDoc::builtin(
        "keymap",
        "[[bindings]]\ncontext = \"rec\"\n[bindings.keys]\n\"j\" = \"rec::noop\"\n",
    )
    .unwrap();
    services.keymap = test_keymap(&services.registry, &[module_doc]);
    (services, log, focus)
}

pub(super) fn open_shell(
    cx: &mut gpui::TestAppContext,
    services: ShellServices,
) -> (gpui::WindowHandle<Root>, gpui::VisualTestContext) {
    open_shell_inner(cx, services, None)
}

/// [`open_shell`] with a real, writable user config directory — the one
/// thing a test of a *persisting* action needs, since every persist path
/// in this crate (`persist_theme`, `keybindings_view::spawn_unbind`, …)
/// silently no-ops when `ShellView::user_dir` is `None`. Pass a
/// `tempfile::TempDir`'s path and keep the `TempDir` alive for the whole
/// test: the file is read back from it after `run_until_parked` drives
/// the background write to completion.
pub(super) fn open_shell_with_user_dir(
    cx: &mut gpui::TestAppContext,
    services: ShellServices,
    user_dir: &std::path::Path,
) -> (gpui::WindowHandle<Root>, gpui::VisualTestContext) {
    open_shell_inner(cx, services, Some(user_dir.to_path_buf()))
}

fn open_shell_inner(
    cx: &mut gpui::TestAppContext,
    services: ShellServices,
    user_dir: Option<PathBuf>,
) -> (gpui::WindowHandle<Root>, gpui::VisualTestContext) {
    cx.update(gpui_component::init);
    cx.update(crate::shell::dialog::init_reclaimed_keybindings);
    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| ShellView::new(services, None, user_dir, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
        })
        .unwrap();
    let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    (window, vcx)
}

pub(super) fn shell_of(
    window: &gpui::WindowHandle<Root>,
    cx: &mut gpui::VisualTestContext,
) -> Entity<ShellView> {
    window.root(cx).unwrap().read_with(cx, |root, _| {
        root.view().clone().downcast::<ShellView>().unwrap()
    })
}

/// Dispatch an action through `ShellView::dispatch`, the common path for keymap,
/// palette, and dialog actions. Tests of palette-only actions can use it to isolate
/// action handling from palette filtering.
pub(super) fn dispatch_action(
    shell: &Entity<ShellView>,
    action: &str,
    cx: &mut gpui::VisualTestContext,
) {
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId(action.to_string()), None, window, cx);
        });
    });
}

/// Open a fresh shell window, draw to install the key-dispatch tree, and return the
/// visual context and shell entity for dock tests.
pub(super) fn dock_test_shell(
    cx: &mut gpui::TestAppContext,
) -> (gpui::VisualTestContext, Entity<ShellView>) {
    cx.update(gpui_component::init);
    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
        })
        .unwrap();
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let root = window.root(&mut cx).unwrap();
    let shell = root.read_with(&cx, |root, _cx| {
        root.view()
            .clone()
            .downcast::<ShellView>()
            .unwrap_or_else(|_| panic!("root view is not a ShellView"))
    });
    (cx, shell)
}
/// Open a real window with a real `ShellView`, draw a frame, dispatch
/// `action`, draw again — the preamble every dialog test here needs.
pub(super) fn dialog_test_shell(
    cx: &mut gpui::TestAppContext,
    action: &str,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    dialog_test_shell_with(cx, test_services(), action)
}
/// [`dialog_test_shell`] over a caller-supplied `ShellServices` — the one
/// thing a dialog whose rows come from *config* needs, since
/// `test_services`' own `Config` is empty (`ConfigSources::default`) and
/// would give the object dialog nothing to list.
pub(super) fn dialog_test_shell_with(
    cx: &mut gpui::TestAppContext,
    services: ShellServices,
    action: &str,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    dialog_test_shell_in(cx, services, None, action)
}
/// [`dialog_test_shell_with`] with a writable user config directory — what
/// a dialog test that asserts on the FILES a verb wrote needs, since
/// `ShellView::user_dir` is `None` in every other fixture and every
/// persist path in this crate skips the write when it is.
///
/// The reload watcher this gives `ShellView` is harmless in a test: it
/// waits on `background_executor().timer`, and `run_until_parked` runs
/// runnable tasks without advancing the clock, so nothing reloads under
/// the assertions unless a test asks it to.
pub(super) fn dialog_test_shell_in_dir(
    cx: &mut gpui::TestAppContext,
    services: ShellServices,
    user_dir: &std::path::Path,
    action: &str,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    dialog_test_shell_in(cx, services, Some(user_dir.to_path_buf()), action)
}
fn dialog_test_shell_in(
    cx: &mut gpui::TestAppContext,
    services: ShellServices,
    user_dir: Option<std::path::PathBuf>,
    action: &str,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    cx.update(gpui_component::init);
    // Same reclaimed keybindings `main` registers in production
    // (`dialog::init_reclaimed_keybindings`'s own doc comment has the
    // full mechanism for each) — without this, a dialog test that
    // presses tab would prove nothing: gpui-component's `Root` would
    // still silently consume it exactly as it does in an unpatched
    // window.
    cx.update(dialog::init_reclaimed_keybindings);
    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| ShellView::new(services, None, user_dir, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
        })
        .unwrap();
    let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let root = window.root(&mut vcx).unwrap();
    let shell = root.read_with(&vcx, |root, _cx| {
        root.view()
            .clone()
            .downcast::<ShellView>()
            .unwrap_or_else(|_| panic!("root view is not a ShellView"))
    });
    vcx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId(action.to_string()), None, window, cx);
        });
    });
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    (shell, vcx)
}
/// Dispatch a platform-shaped double-click: down/up at `click_count` 1,
/// then down/up at `click_count` 2, all at one point with the given
/// modifiers, with a draw between the two clicks (the OS delivers them
/// across frames). The first click is an ordinary down — on a dialog row
/// it selects — so the second click, the one carrying `click_count: 2`,
/// is what a double-click door sees. Shared by the fullscreen and the
/// dialog double-click tests.
pub(super) fn double_click(
    cx: &mut gpui::VisualTestContext,
    at: gpui::Point<gpui::Pixels>,
    modifiers: gpui::Modifiers,
) {
    for count in 1..=2 {
        cx.update(|window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: at,
                    modifiers,
                    click_count: count,
                    first_mouse: false,
                }),
                cx,
            );
            window.dispatch_event(
                gpui::PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position: at,
                    modifiers,
                    click_count: count,
                }),
                cx,
            );
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
}

/// Does the shared dialog filter currently hold focus?
pub(super) fn dialog_filter_is_focused(
    shell: &Entity<ShellView>,
    cx: &mut gpui::VisualTestContext,
) -> bool {
    cx.update(|window, cx| {
        shell
            .read(cx)
            .dialog_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
    })
}
/// Whether the scope bar's live text field currently holds focus.
pub(super) fn filter_is_focused(
    shell: &Entity<ShellView>,
    cx: &mut gpui::VisualTestContext,
) -> bool {
    cx.update(|window, cx| {
        shell
            .read(cx)
            .filter_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
    })
}
/// Add a test-only `g g` sequence so pending-keystroke and which-key tests have a
/// sequence to exercise. The builtin keymap has none.
pub(super) fn test_services_with_gg_binding() -> ShellServices {
    let mut services = test_services();
    services
        .registry
        .register(crate::actions::ActionDef {
            id: crate::actions::ActionId("test::gg".to_string()),
            title: "Test gg".to_string(),
            category: "Test".to_string(),
        })
        .unwrap();
    let user_doc = LayerDoc {
        layer: geode_core::config::Layer::User,
        name: "keymap".to_string(),
        file: "<test:user>".into(),
        table: "[[bindings]]\n[bindings.keys]\n\"g g\" = \"test::gg\"\n"
            .parse()
            .unwrap(),
    };
    services.keymap = test_keymap(&services.registry, &[user_doc]);
    services
}
/// Call the real `apply_reload` path through the shell entity. The test executor's
/// `run_until_parked` does not advance its virtual clock, so direct application
/// isolates reload behavior from the polling timer.
pub(super) fn config_with_mod(mod_key: &str) -> Config {
    Config::load(&ConfigSources {
        builtin: vec![
            LayerDoc::builtin("app", &format!("[keymap]\nmod = \"{mod_key}\"\n")).unwrap(),
        ],
        desk: None,
        user: None,
    })
}

mod addfilter;
mod asof;
mod autosize;
mod chrome_and_dialogs;
mod commandline;
mod diagnostics;
mod dialog_stack;
mod dock;
mod drag;
mod flip;
mod grouping;
mod input;
mod keybindings_dialog;
mod launch;
mod objectdialog;
mod occupants;
mod palette;
mod perf;
mod picker;
mod pin;
mod reload;
mod scope_expr;
mod scopebar;
mod session;
mod stacks;
mod tilepicker;
mod tiling_keys;
