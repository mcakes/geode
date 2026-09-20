//! Shared test scaffolding for `ShellView`'s test suite (moved out of
//! `shell/mod.rs`, Task 0 part 2 — a pure move). Each seam below is one
//! test file, in the order the tests used to sit in the single file;
//! helpers used by more than one seam live here as `pub(super) fn`.

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
// `WindowExt` and (since Task 4) `Focusable` are already brought in by
// `use super::*` (top-of-file imports in `shell/mod.rs`) — needed by
// `handle_key_down`'s dialog guard and `filter_is_focused`/
// `dialog_filter_is_focused`'s `.focus_handle(cx)` calls below,
// respectively.

/// The test layer every shell fixture stacks on `BUILTIN_KEYMAP`: the
/// shipped keymap has no create-a-tile chord any more (spec 2026-09-08
/// add-tile §3.1 — tiles are added by kind from the palette), so the
/// ~80 tests that say `ctrl-v`/`ctrl-h` keep meaning "add a recorder
/// tile, side by side / below" through these two bindings. They are
/// exactly the shape a desk keymap would ship (`tile::add_<kind>_*`),
/// not a private test-only action.
pub(super) const TEST_ADD_KEYMAP: &str = "[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"ctrl+v\" = \"tile::add_rec_horizontal\"\n\"ctrl+h\" = \"tile::add_rec_vertical\"\n";

/// `BUILTIN_KEYMAP` + [`TEST_ADD_KEYMAP`] + `extra`, built clean.
pub(super) fn test_keymap(registry: &ActionRegistry, extra: &[LayerDoc]) -> crate::keymap::Keymap {
    test_keymap_with_fragments(registry, &[], extra)
}

/// [`test_keymap`] with a roster's keymap fragments spliced in at the
/// point `main.rs` splices them (market-data documents §8.4) — above the
/// compiled-in docs, below everything a trader edits. Every fixture here
/// goes through this call with an empty fragment list, so the identity
/// case is exercised by the whole suite and only a fragment test has to
/// name it.
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

/// The shell fixtures' one roster: a `RecordingFactory` of kind "rec" —
/// the kind [`TEST_ADD_KEYMAP`]'s `ctrl+v`/`ctrl+h` add, so a shell
/// built here really can add a tile twice and get two tiles (an add
/// onto a *placeholder* fills it in place, spec 2026-09-08 add-tile
/// §4.2, so a roster with no "rec" would collapse every second add into
/// the first tile). There is no default kind (§7.1): a tile created by
/// some *other* path — a direct `Workspaces::split_active`, or a session
/// record naming a kind nothing registered — is a placeholder.
fn services_with_rec_roster() -> (
    ShellServices,
    std::rc::Rc<std::cell::RefCell<Vec<crate::module::recording::Recorded>>>,
    RecFocus,
) {
    let (services, log, focus, _input) = services_with_rec_roster_shipping(None);
    (services, log, focus)
}

/// The default bindings the fixture recorder ships when asked to
/// (market-data documents §8.4): `q` in its own `rec` context, so a
/// hosting test can prove a key bound by nothing but the *module* reaches
/// the module — `TEST_ADD_KEYMAP` and the shell's own `BUILTIN_KEYMAP`
/// bind nothing to `q`, in any context.
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

/// The fixture for insert mode (market-data spec §8.6): the recorder's
/// own fragment, in the exact shape the market-data panel's will take —
/// one table per mode, so the two vocabularies cannot bleed into each
/// other. `i` opens the tile-owned input (and with it insert mode), `j`
/// is the normal-mode motion that must NOT fire while a trader is typing
/// in it, and `escape`/`enter` are the only two keys the module claims
/// back while it does.
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

fn services_with_rec_roster_shipping(
    fragment: Option<&'static str>,
) -> (
    ShellServices,
    std::rc::Rc<std::cell::RefCell<Vec<crate::module::recording::Recorded>>>,
    RecFocus,
    RecInput,
) {
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
    // Same reasoning as `register_pick_actions` just above, for the
    // `scope::<name>` actions (Phase 4a §3.11) — exercised here even
    // though this config has no `[scopes]` doc, so `crate::shell::
    // saved_scopes` returns empty and the loop is a no-op, but the
    // startup-ordering path itself still runs on every test built from
    // this fixture.
    register_scope_actions(&mut registry, &crate::shell::saved_scopes(&config, false));
    // The add rows for the recorder kind the shell tests use (spec
    // 2026-09-08 add-tile §3.2) — `main.rs` registers these from the
    // roster's kinds in this same slot, before `build_keymap`.
    crate::defaults::register_add_actions(&mut registry, &["rec"]);
    let mut recorder = crate::module::recording::RecordingFactory::new("rec");
    recorder.fragment = fragment;
    let log = recorder.log.clone();
    let last_focus = recorder.last_focus.clone();
    let input = recorder.input.clone();
    let mut roster = crate::module::ModuleRoster::new();
    roster.add(Box::new(recorder));
    // Module actions exist before `build_keymap`, exactly as `main.rs`
    // orders it — a binding into the module's own context is what
    // `services_with_recorder`'s extra layer needs to resolve.
    roster.register_actions(&mut registry);
    // And the modules' fragments are collected right after, from the
    // finished roster, in `main.rs`'s own order (§8.4).
    // Not asserted clean here: one fixture below ships a fragment that
    // is MEANT to have a binding dropped. The callers assert what they
    // expect instead (`services_with_a_module_fragment` clean, the
    // dropping fixture exactly one error).
    let (keymap_fragments, keymap_fragment_diagnostics) = roster.keymap_fragments();
    let mod_alias = default_mod();
    let keymap = test_keymap_with_fragments(&registry, &keymap_fragments, &[]);
    let (theme, warnings) = crate::theme::load_bundled();
    assert!(warnings.is_empty(), "{warnings:?}");
    let services = ShellServices {
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
    };
    (services, log, last_focus, input)
}

/// [`test_services`] plus a binding into the recording module's own key
/// context, so a key can be seen to reach an occupant. The roster is the
/// same one every fixture here builds — a "rec" factory and no default
/// kind (spec 2026-09-08 add-tile §7.1).
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

/// Dispatch `action` through `ShellView::dispatch` directly — the same
/// route `dialog_test_shell_in`'s own preamble uses to open the
/// keybinding/settings/object dialogs, reused here for a palette-only
/// action with no keymap binding of its own (`frame::pick_book`,
/// `scope::save_current`, …) rather than driving the palette's own
/// filter-and-enter dance for no benefit over calling the one method
/// every dispatch route already funnels through. Hoisted here (review
/// finding) after `shell::tests::picker` and `shell::tests::objectdialog`
/// each carried an identical private copy.
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

/// Shared scaffolding for the dock e2e tests below (dock-regions task):
/// open a window over a fresh `ShellView`, draw once so the key
/// dispatch tree exists, and hand back the visual context plus the
/// downcast shell entity — the exact setup every other e2e test here
/// builds inline.
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
/// Does the scope bar's live text field (Task 4, spec §3.11) currently
/// hold focus?
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
/// Layers a test-only `"g g"` sequence binding on top of the builtin
/// keymap (spec §3.4: sequence bindings), so the status bar's
/// pending-keystroke display (Task 4) has something real to show. The
/// builtin keymap has no sequence bindings anymore (move-tile went
/// direct to `ctrl+alt+arrows`), so this isolated binding is the way
/// tests exercise a pending keystroke at all.
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
/// `apply_reload` is `ShellView`'s real config-hot-reload apply path
/// (Task 1c-1); the watcher task is just what schedules calling it —
/// gpui's test executor never advances its simulated clock on
/// `run_until_parked` (confirmed against the pinned rev's
/// `TestScheduler::run`, which is a plain `while step() {}` with no
/// clock advancement), so there's no practical way to drive a ~500ms
/// polling loop through a `#[gpui::test]`. These tests call
/// `apply_reload` directly through the real entity instead — still a
/// real-entity test, exercising the exact method the watcher calls.
pub(super) fn config_with_mod(mod_key: &str) -> Config {
    Config::load(&ConfigSources {
        builtin: vec![
            LayerDoc::builtin("app", &format!("[keymap]\nmod = \"{mod_key}\"\n")).unwrap(),
        ],
        desk: None,
        user: None,
    })
}

mod asof;
mod chrome_and_dialogs;
mod commandline;
mod diagnostics;
mod dock;
mod drag;
mod flip;
mod grouping;
mod input;
mod keybindings_dialog;
mod objectdialog;
mod occupants;
mod palette;
mod perf;
mod picker;
mod reload;
mod scope_expr;
mod scopebar;
mod session;
mod stacks;
mod tilepicker;
mod tiling_keys;
