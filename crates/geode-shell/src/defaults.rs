//! Compiled-in defaults: the builtin action set and keymap (spec §3.1).
//! These form the Builtin config layer; desk and user files override them.

use crate::actions::{ActionDef, ActionId, ActionRegistry};
use crate::keymap::Modifiers;
use geode_core::config::{Config, Diagnostic, Severity};

/// The builtin keymap document, layered under desk/user keymaps.
///
/// Directional focus is `mod+h/j/k/l` (user direction — vim letters on
/// the primary modifier, replacing the Phase 1c `ctrl+w h/j/k/l` chords).
/// Not `ctrl+arrows`: macOS binds those to Mission Control/Spaces
/// system-wide and swallows the events before any app sees them.
/// Move-tile is `ctrl+alt+arrows` (user direction, superseding the earlier
/// `ctrl+shift+arrows`) and resize is `shift+arrows` (user direction,
/// retiring the Phase 1c `ctrl+w shift+h/j/k/l` vim window prefix and
/// `shift+h/j/k/l`) — the builtin keymap now has no sequence
/// bindings at all; sequences remain a first-class engine feature for
/// desk/user layers. There is no split chord: tiles are *added* by kind
/// (`tile::add_<kind>[_horizontal|_vertical]`, palette rows registered
/// by `register_add_actions`) and `shift+d`/`ctrl+shift+d` duplicate the
/// focused tile beside/below itself (spec 2026-09-08 add-tile §3).
/// `ctrl+v` and `ctrl+h` are free. Resize is a direct binding, not a
/// mode: `shift+arrows` move the
/// divider adjacent to the focused tile toward the arrow's direction by
/// `RESIZE_STEP` (the key names the divider's direction, not
/// "grow"; see [`crate::tiling::Tree::move_divider`] for the edge-flip
/// consequence when the focused tile has no divider on that side).
///
/// Docks (dock-regions task): `ctrl+[` / `ctrl+]` / `ctrl+/` toggle the
/// left/right/bottom dock; the *move*-to-dock verbs are the same physical
/// keys with shift held — which the user thinks of as `ctrl+shift+[` etc.,
/// but which are deliberately bound as `ctrl+{` / `ctrl+}` / `ctrl+?`
/// (shifted character, NO shift modifier). Verified against the pinned
/// platform sources, not assumed: both macOS (`gpui_macos/src/events.rs`,
/// the `else if shift { shift = false; chars_with_shift }` arm) and Windows
/// (`gpui_windows/src/keyboard.rs`, `get_keystroke_key`'s
/// `need_to_convert_to_shifted_key` OEM-key list) deliver shift+punctuation
/// as the shifted character with the shift modifier *cleared* — a real
/// `KeyDownEvent` for shift+[ arrives as key `{`, `shift: false`, so a
/// `"ctrl+shift+["` binding would never match anything. (Letters are the
/// opposite: a shifted letter stays the letter + shift modifier, so a
/// shifted-letter chord would be bound with the modifier spelled out —
/// as would a shifted *arrow*, which is why `shift+arrows` resize binds
/// shift as a modifier.) The e2e dock tests dispatch `ctrl-{`
/// through gpui's real pipeline to pin this shape.
///
/// Close-tile is `ctrl+w` (user direction — the browser/vim close idiom;
/// free since the vim window prefix retired, and unclaimed by macOS).
///
/// Frame slots (Phase 3 §4.2) are declared in their own table, FIRST —
/// deliberately ahead of the `workspace` table's `mod+1..9` below, not
/// alongside the other context-less bindings further down. The matcher
/// keeps the *last* declaration-order match among exact keystroke ties
/// (`Matcher::press`, "Bindings are in layer-then-definition order; keep
/// the last"). This ordering was originally defense-in-depth against a
/// user's `keymap.mod = "ctrl"` aliasing `mod+1` onto the exact same
/// keystroke as the shipped `ctrl+1`: Task 4b (Phase 4a, user ruling)
/// refused that alias outright as invalid config instead of relying on
/// tie-break ordering to keep it safe (`defaults::mod_alias_from_config`
/// returns an error diagnostic and keeps the default alias) — `mod` and
/// `ctrl` can no longer collide, so this file's declaration order is no
/// longer load-bearing for that reason. It stays as written: removing it
/// buys nothing, and the tie-break rule is worth keeping documented for
/// whatever binding table lands here next.
pub const BUILTIN_KEYMAP: &str = r#"
[[bindings]]
[bindings.keys]
"ctrl+1" = "frame::slot_1"
"ctrl+2" = "frame::slot_2"
"ctrl+3" = "frame::slot_3"
"ctrl+4" = "frame::slot_4"
"ctrl+5" = "frame::slot_5"
"ctrl+6" = "frame::slot_6"
"ctrl+7" = "frame::slot_7"
"ctrl+8" = "frame::slot_8"
"ctrl+9" = "frame::slot_9"
"ctrl+0" = "frame::slot_clear"

[[bindings]]
context = "workspace"
[bindings.keys]
"mod+h" = "workspace::focus_left"
"mod+j" = "workspace::focus_down"
"mod+k" = "workspace::focus_up"
"mod+l" = "workspace::focus_right"
"ctrl+alt+left" = "workspace::move_left"
"ctrl+alt+down" = "workspace::move_down"
"ctrl+alt+up" = "workspace::move_up"
"ctrl+alt+right" = "workspace::move_right"
"shift+left" = "workspace::resize_left"
"shift+down" = "workspace::resize_down"
"shift+up" = "workspace::resize_up"
"shift+right" = "workspace::resize_right"
"mod+e" = "workspace::toggle_split_orientation"
"mod+f" = "workspace::fullscreen_tile"
"ctrl+w" = "workspace::close_tile"
"shift+d" = "workspace::duplicate_horizontal"
"ctrl+shift+d" = "workspace::duplicate_vertical"
"ctrl+[" = "dock::toggle_left"
"ctrl+]" = "dock::toggle_right"
"ctrl+/" = "dock::toggle_bottom"
"ctrl+{" = "dock::move_left"
"ctrl+}" = "dock::move_right"
"ctrl+?" = "dock::move_bottom"
"mod+1" = "workspace::switch_1"
"mod+2" = "workspace::switch_2"
"mod+3" = "workspace::switch_3"
"mod+4" = "workspace::switch_4"
"mod+5" = "workspace::switch_5"
"mod+6" = "workspace::switch_6"
"mod+7" = "workspace::switch_7"
"mod+8" = "workspace::switch_8"
"mod+9" = "workspace::switch_9"

[[bindings]]
[bindings.keys]
"ctrl+k" = "palette::toggle"
"ctrl+shift+p" = "palette::toggle"
"mod+shift+t" = "theme::toggle_mode"
"ctrl+," = "settings::open"
"ctrl+=" = "fontsize::increase"
"ctrl+-" = "fontsize::decrease"
"mod+shift+p" = "perf::toggle_overlay"
"mod+z" = "frame::scope_undo"
"mod+shift+z" = "frame::scope_redo"
"mod+/" = "frame::focus_text"
"mod+p" = "frame::pick"
"mod+t" = "frame::as_of"

[[bindings]]
context = "tile"
[bindings.keys]
":" = "tile::command_line"
"/" = "tile::find"

[[bindings]]
context = "blotter && mode == normal"
[bindings.keys]
"j" = "blotter::down"
"k" = "blotter::up"
"h" = "blotter::left"
"l" = "blotter::right"
"g g" = "blotter::top"
"shift+g" = "blotter::bottom"
"ctrl+d" = "blotter::page_down"
"ctrl+u" = "blotter::page_up"
"home" = "blotter::first_col"
"end" = "blotter::last_col"
"z o" = "blotter::expand"
"z c" = "blotter::collapse"
"z a" = "blotter::toggle"
"z shift+r" = "blotter::expand_all"
"z shift+m" = "blotter::collapse_all"
"enter" = "blotter::toggle"
"v" = "blotter::visual"
"y" = "blotter::yank"
"n" = "blotter::find_next"
"shift+n" = "blotter::find_prev"
"s" = "blotter::sort_cycle"
"escape" = "blotter::escape"

[[bindings]]
context = "blotter && mode == visual"
[bindings.keys]
"j" = "blotter::down"
"k" = "blotter::up"
"g g" = "blotter::top"
"shift+g" = "blotter::bottom"
"ctrl+d" = "blotter::page_down"
"ctrl+u" = "blotter::page_up"
"y" = "blotter::yank"
"v" = "blotter::escape"
"escape" = "blotter::escape"

[[bindings]]
context = "diagnostics"
[bindings.keys]
"j" = "diagnostics::down"
"k" = "diagnostics::up"
"g g" = "diagnostics::top"
"shift+g" = "diagnostics::bottom"
"ctrl+d" = "diagnostics::page_down"
"ctrl+u" = "diagnostics::page_up"
"[" = "diagnostics::prev_section"
"]" = "diagnostics::next_section"
"z o" = "diagnostics::expand"
"z c" = "diagnostics::collapse"
"#;

/// Mirrors `geode_blotter::tile::ACTIONS` (id, title) exactly — the
/// shell cannot depend on the blotter crate, so `BUILTIN_KEYMAP`'s own
/// `blotter::*` bindings (above) carry their own copy of the ids they
/// name, registered here so `build_keymap` never has to drop them for
/// want of a registered action, even in a shell-only build that has
/// never loaded `geode-blotter`. When `BlotterFactory::register_actions`
/// (Phase 3 §3.2) later tries to register the same ids against the
/// same registry, `ActionRegistry::register` reports each as already
/// registered — an `Err` it already discards — so the titles below are
/// the ones actually shown in the palette; keep them identical to
/// `geode_blotter::tile::ACTIONS`. `pub` (final review) so
/// `geode-blotter`'s own `the_shells_reserved_blotter_actions_match_ours`
/// test can compare titles as well as ids — the shell cannot depend on
/// the blotter crate to run that check itself, so the mirroring only
/// runs from the blotter side.
pub const BLOTTER_ACTION_DEFS: &[(&str, &str)] = &[
    ("blotter::down", "Cursor down"),
    ("blotter::up", "Cursor up"),
    ("blotter::left", "Cursor left"),
    ("blotter::right", "Cursor right"),
    ("blotter::top", "Cursor to top"),
    ("blotter::bottom", "Cursor to bottom"),
    ("blotter::page_down", "Half page down"),
    ("blotter::page_up", "Half page up"),
    ("blotter::first_col", "First column"),
    ("blotter::last_col", "Last column"),
    ("blotter::expand", "Expand node"),
    ("blotter::collapse", "Collapse node"),
    ("blotter::toggle", "Toggle node"),
    ("blotter::expand_all", "Expand all"),
    ("blotter::collapse_all", "Collapse all"),
    ("blotter::visual", "Visual mode"),
    ("blotter::escape", "Leave visual / clear narrowing"),
    ("blotter::yank", "Yank rows as TSV"),
    ("blotter::find_next", "Next match"),
    ("blotter::find_prev", "Previous match"),
    ("blotter::sort_cycle", "Sort by cursor column"),
];

/// Just the ids from [`BLOTTER_ACTION_DEFS`], for the mirror test.
pub const BLOTTER_ACTIONS: &[&str] = &[
    "blotter::down",
    "blotter::up",
    "blotter::left",
    "blotter::right",
    "blotter::top",
    "blotter::bottom",
    "blotter::page_down",
    "blotter::page_up",
    "blotter::first_col",
    "blotter::last_col",
    "blotter::expand",
    "blotter::collapse",
    "blotter::toggle",
    "blotter::expand_all",
    "blotter::collapse_all",
    "blotter::visual",
    "blotter::escape",
    "blotter::yank",
    "blotter::find_next",
    "blotter::find_prev",
    "blotter::sort_cycle",
];

/// Mirrors `geode_diagnostics::ACTIONS` (id, title) exactly — same
/// reasoning as [`BLOTTER_ACTION_DEFS`]'s own doc comment: `BUILTIN_KEYMAP`'s
/// `diagnostics::*` bindings carry their own copy of the ids they name,
/// registered here so `build_keymap` never drops them for want of a
/// registered action even in a shell-only build that never loaded
/// `geode-diagnostics` — `cargo check -p geode-shell --features
/// test-support --all-targets` is exactly that build. `pub` for the same
/// cross-crate mirror-test reason. `diagnostics::open` was retired by user
/// ruling 2026-09-09: the status bar's diagnostics-summary click is the
/// one focus-or-add door (`ShellView::open_module`), and the palette's
/// `Diagnostics: Split` rows (`register_add_actions`) always add or fill.
pub const DIAGNOSTICS_ACTION_DEFS: &[(&str, &str)] = &[
    ("diagnostics::down", "Cursor down"),
    ("diagnostics::up", "Cursor up"),
    ("diagnostics::top", "Cursor to top"),
    ("diagnostics::bottom", "Cursor to bottom"),
    ("diagnostics::page_down", "Half page down"),
    ("diagnostics::page_up", "Half page up"),
    ("diagnostics::next_section", "Next section"),
    ("diagnostics::prev_section", "Previous section"),
    ("diagnostics::expand", "Expand"),
    ("diagnostics::collapse", "Collapse"),
];

/// Just the ids from [`DIAGNOSTICS_ACTION_DEFS`], for the mirror test.
pub const DIAGNOSTICS_ACTIONS: &[&str] = &[
    "diagnostics::down",
    "diagnostics::up",
    "diagnostics::top",
    "diagnostics::bottom",
    "diagnostics::page_down",
    "diagnostics::page_up",
    "diagnostics::next_section",
    "diagnostics::prev_section",
    "diagnostics::expand",
    "diagnostics::collapse",
];

fn action(reg: &mut ActionRegistry, id: &str, title: &str, category: &str) {
    reg.register(ActionDef {
        id: ActionId(id.to_string()),
        title: title.to_string(),
        category: category.to_string(),
    })
    .expect("builtin action ids are unique by construction");
}

/// Register the shell's own actions. Modules register theirs at module
/// registration time (spec §9.1); these are the shell's.
pub fn register_builtin_actions(reg: &mut ActionRegistry) {
    action(reg, "workspace::focus_left", "Focus left", "Workspace");
    action(reg, "workspace::focus_down", "Focus down", "Workspace");
    action(reg, "workspace::focus_up", "Focus up", "Workspace");
    action(reg, "workspace::focus_right", "Focus right", "Workspace");
    action(reg, "workspace::move_left", "Move left", "Workspace");
    action(reg, "workspace::move_down", "Move down", "Workspace");
    action(reg, "workspace::move_up", "Move up", "Workspace");
    action(reg, "workspace::move_right", "Move right", "Workspace");
    action(
        reg,
        "workspace::resize_left",
        "Move split left",
        "Workspace",
    );
    action(
        reg,
        "workspace::resize_down",
        "Move split down",
        "Workspace",
    );
    action(reg, "workspace::resize_up", "Move split up", "Workspace");
    action(
        reg,
        "workspace::resize_right",
        "Move split right",
        "Workspace",
    );
    // Pairwise reorient around the focused tile (see
    // `Tree::toggle_split_orientation` — deliberately not i3's
    // whole-container toggle), bound mod+e (i3's layout-toggle key).
    action(
        reg,
        "workspace::toggle_split_orientation",
        "Toggle split orientation",
        "Workspace",
    );
    // Duplicate the focused tile beside itself, carrying its serialized
    // state (spec 2026-09-08 add-tile §3.3/§6). Horizontal = to the
    // right, Vertical = below — the tree's own orientation words.
    action(
        reg,
        "workspace::duplicate_horizontal",
        "Duplicate tile horizontal",
        "Workspace",
    );
    action(
        reg,
        "workspace::duplicate_vertical",
        "Duplicate tile vertical",
        "Workspace",
    );
    action(
        reg,
        "workspace::fullscreen_tile",
        "Fullscreen tile",
        "Workspace",
    );
    action(reg, "workspace::close_tile", "Close tile", "Workspace");
    // Dock regions (dock-regions task): toggle shows/hides a dock (a
    // hidden dock keeps its tile); move sends the focused tile there —
    // or back into the tree when it's already the focused dock. See
    // `tiling::Workspace` for the full verb semantics and BUILTIN_KEYMAP's
    // doc comment for why the move bindings are spelled `ctrl+{` etc.
    action(reg, "dock::toggle_left", "Toggle left dock", "Dock");
    action(reg, "dock::toggle_right", "Toggle right dock", "Dock");
    action(reg, "dock::toggle_bottom", "Toggle bottom dock", "Dock");
    action(reg, "dock::move_left", "Move tile to left dock", "Dock");
    action(reg, "dock::move_right", "Move tile to right dock", "Dock");
    action(reg, "dock::move_bottom", "Move tile to bottom dock", "Dock");
    for i in 1..=9 {
        action(
            reg,
            &format!("workspace::switch_{i}"),
            &format!("Switch to workspace {i}"),
            "Workspace",
        );
    }
    action(reg, "palette::toggle", "Toggle command palette", "Palette");
    action(
        reg,
        "theme::toggle_mode",
        "Toggle light/dark theme",
        "Theme",
    );
    // The sidebar's bottom profile icon, ctrl+,, and the palette all
    // dispatch this (Task 5: the real settings dialog). Category
    // "Appearance" — not a standalone "Settings" category — groups it in
    // the palette alongside the theme rows, which share the same category
    // (`palette::THEME_CATEGORY`).
    action(reg, "settings::open", "Open settings", "Appearance");
    // Step the UI font size (crate::fontsize, clamped small..=large).
    // ctrl+= / ctrl+- — the browser-zoom idiom: the unshifted key next to
    // backspace is `=`, and platforms deliver ctrl+that-key as key "="
    // (see the shift+punctuation note on BUILTIN_KEYMAP's doc comment for
    // why "ctrl++" would be both unpressable-without-shift and unparseable
    // — parse_keystroke splits on '+'). Same "Appearance" category as the
    // settings dialog that owns the equivalent toggle group.
    action(
        reg,
        "fontsize::increase",
        "Increase font size",
        "Appearance",
    );
    action(
        reg,
        "fontsize::decrease",
        "Decrease font size",
        "Appearance",
    );
    // The keybinding dialog (Part B). Palette-only by design: no key
    // binding of its own in BUILTIN_KEYMAP — bootstrapping a dialog whose
    // whole purpose is showing/editing keybindings out of a keybinding
    // would be a little too cute, and the palette is always reachable
    // regardless. Category "Keyboard", not "Appearance" — this edits
    // behavior (bindings), not how the app looks.
    action(reg, "keybindings::open", "Keyboard shortcuts", "Keyboard");
    // Phase 4c: the object dialog over `views` (`shell::objectdialog`).
    // Palette-only, with no binding in BUILTIN_KEYMAP, for the same
    // reason `keybindings::open` above has none: it is an occasional,
    // deliberate act of config maintenance rather than muscle memory
    // worth a chord, and the palette is always reachable. Category
    // "Configuration" — its siblings (`config::groupings`,
    // `config::sources`, `config::scopes`, the schema inspector) land
    // beside it as their adapters are built, so the palette groups the
    // whole family under one heading.
    action(reg, "config::views", "Edit views", "Configuration");
    // Frame-time instrumentation (spec §7.4). The overlay toggle is bound
    // `mod+shift+p` ("performance" — a shifted letter keeps its modifier,
    // unlike the punctuation story above, so this spelling is real, and no
    // builtin binding claims it under the default mod). This used to be a
    // remap hazard: under `keymap.mod = "ctrl"` it aliased onto
    // `ctrl+shift+p`, and the matcher's last-exact-match rule let it
    // shadow the palette's second binding (ctrl+k still opened the
    // palette). Task 4b (Phase 4a, user ruling) refused that alias
    // outright as invalid config, so `mod` and `ctrl` can no longer
    // collide here or anywhere else. `perf::reset` is palette-only —
    // resetting counters is an occasional deliberate act, not muscle
    // memory worth a chord.
    action(
        reg,
        "perf::toggle_overlay",
        "Toggle performance overlay",
        "Diagnostics",
    );
    action(
        reg,
        "perf::reset",
        "Reset performance counters",
        "Diagnostics",
    );
    // The per-tile command line (Phase 3 §3.4): the shell's own actions,
    // bound `:`/`/` in the `tile` context (any focused tile with an
    // occupant) so every module gets them for free, without registering
    // anything of its own.
    action(
        reg,
        "tile::command_line",
        "Open the tile command line",
        "Tile",
    );
    action(reg, "tile::find", "Find in tile", "Tile");
    // The nine grouping slots (Phase 3 §4.2): ctrl+1..9 activate a
    // configured slot (an empty one is ignored — see `Frame::
    // set_active_slot`), ctrl+0 returns every following tile to its
    // view's own grouping.
    for i in 1..=9 {
        action(
            reg,
            &format!("frame::slot_{i}"),
            &format!("Grouping slot {i}"),
            "Frame",
        );
    }
    action(
        reg,
        "frame::slot_clear",
        "Clear grouping slot (views' own grouping)",
        "Frame",
    );
    // Scope undo/redo/clear (Phase 4a §3.6): `mod+z`/`mod+shift+z` walk
    // the bounded undo/redo stacks `Frame::set_scope` maintains;
    // `scope_clear` is palette-only, like `perf::reset` above — clearing
    // the whole scope is an occasional deliberate act, not muscle memory
    // worth a chord of its own.
    action(reg, "frame::scope_undo", "Undo scope change", "Frame");
    action(reg, "frame::scope_redo", "Redo scope change", "Frame");
    action(reg, "frame::scope_clear", "Clear scope", "Frame");
    // The dimension picker (Phase 4a §3.3), opened on the column-choice
    // stage — `mod+p`. The per-column `frame::pick_<column>` actions
    // (opening straight onto one column's values stage) are registered
    // separately, from the loaded schema, by `register_pick_actions`
    // below: this crate cannot register them here because there is no
    // schema to enumerate at `register_builtin_actions`' own call site
    // (before any config is loaded).
    action(reg, "frame::pick", "Pick a dimension", "Frame");
    // The scope bar's live text field (Phase 4a §3.11): `mod+/` moves
    // focus into it from anywhere in the shell, the one keyboard route
    // in (typing itself, once focused, needs no action — the field's own
    // `Input` handles that; see `shell/input.rs`'s filter-focused guard).
    action(
        reg,
        "frame::focus_text",
        "Focus the scope text field",
        "Frame",
    );
    // The as-of selector (Phase 4a §3.6): `mod+t` opens a modal to view
    // data as of a past instant, with the frame's recent generation
    // times as honest presets. `frame::live`/`frame::as_of_undo` are
    // palette-only — occasional deliberate acts, not muscle memory worth
    // a chord of their own, same reasoning as `frame::scope_clear`/
    // `perf::reset` above.
    action(reg, "frame::as_of", "Jump to a point in time", "Frame");
    action(reg, "frame::live", "Return to live", "Frame");
    action(
        reg,
        "frame::as_of_undo",
        "Swap to the previous as of",
        "Frame",
    );
    // Profiler-feature actions (the `profiling` feature — gpui's own
    // `profiler` histograms/overlay): registered only when compiled in,
    // so the palette never advertises a no-op.
    #[cfg(feature = "profiling")]
    {
        action(
            reg,
            "perf::gpui_overlay",
            "Cycle gpui frame overlay",
            "Diagnostics",
        );
        action(
            reg,
            "perf::dump",
            "Dump frame-time stats to stderr",
            "Diagnostics",
        );
    }
    // The blotter's own actions (Phase 3 §3.2), reserved here so
    // BUILTIN_KEYMAP's `blotter::*` bindings above never get dropped as
    // unregistered — see `BLOTTER_ACTION_DEFS`'s doc comment.
    for (id, title) in BLOTTER_ACTION_DEFS {
        action(reg, id, title, "Blotter");
    }
    // The diagnostics module's own actions (Phase 4b Task 5), reserved
    // here so BUILTIN_KEYMAP's `diagnostics::*` bindings never get dropped
    // as unregistered — see `DIAGNOSTICS_ACTION_DEFS`'s doc comment.
    for (id, title) in DIAGNOSTICS_ACTION_DEFS {
        action(reg, id, title, "Diagnostics");
    }
}

/// `frame::pick_<column>` for every pickable column (Phase 4a §3.3).
/// Registered at startup from the loaded schema — `main.rs` calls this
/// right after [`register_builtin_actions`] and before `build_keymap`,
/// over `shell::pickable_columns(&config)` — so the palette lists `Pick:
/// <column>` for each one and a keymap can bind e.g. `mod+b =
/// "frame::pick_book"`. Unlike `register_builtin_actions`, the action ids
/// this produces depend on config, so `test_services` (the shell crate's
/// own test fixture) calls this too, over its (empty) config, so the
/// startup-ordering path is exercised even when there is nothing to
/// register — the loop below is simply a no-op then.
pub fn register_pick_actions(reg: &mut ActionRegistry, columns: &[crate::shell::Pickable]) {
    for c in columns {
        action(
            reg,
            &format!("frame::pick_{}", c.column),
            &format!("Pick: {}", c.column),
            "Frame",
        );
    }
}

/// `scope::<name>` for every saved scope (spec §3.11), category "Scope",
/// unbound by default — the same shape as [`register_pick_actions`],
/// registered right beside it: `main.rs` calls this after it, over
/// `shell::saved_scopes(&config)`, so the palette lists one entry per
/// saved scope and a keymap can bind e.g. `mod+shift+e =
/// "scope::eu"`. Like `register_pick_actions`, the action ids this
/// produces depend on config — a scope added by a live reload is not
/// registered until restart (spec §1.3) — so `test_services` calls this
/// too, over its (empty) saved scopes, exercising the startup-ordering
/// path even when there is nothing to register.
pub fn register_scope_actions(reg: &mut ActionRegistry, saved: &geode_core::scopes::SavedScopes) {
    for name in saved.keys() {
        action(
            reg,
            &format!("scope::{name}"),
            &format!("Scope: {name}"),
            "Scope",
        );
    }
}

/// Three palette rows per module kind (spec 2026-09-08 add-tile §3.2,
/// retitled by user ruling 2026-09-09 to the crate's `Category: Verb`
/// pattern — see `register_pick_actions`'s "Pick: <column>" and
/// `register_scope_actions`'s "Scope: <name>"): `tile::add_<kind>`
/// ("<Kind>: Split", the setting decides the direction),
/// `tile::add_<kind>_horizontal` ("<Kind>: Split Horizontal", to the
/// right) and `tile::add_<kind>_vertical` ("<Kind>: Split Vertical",
/// below), category "Tiles". Action ids are unchanged — only titles.
/// Registered from the roster's kinds right beside `register_pick_actions`
/// / `register_scope_actions` — after the builtins, before `build_keymap`
/// — so a desk keymap can bind e.g. `mod+b = "tile::add_blotter"`.
pub fn register_add_actions(reg: &mut ActionRegistry, kinds: &[&str]) {
    for kind in kinds {
        let title = capitalize(kind);
        action(
            reg,
            &format!("tile::add_{kind}"),
            &format!("{title}: Split"),
            "Tiles",
        );
        action(
            reg,
            &format!("tile::add_{kind}_horizontal"),
            &format!("{title}: Split Horizontal"),
            "Tiles",
        );
        action(
            reg,
            &format!("tile::add_{kind}_vertical"),
            &format!("{title}: Split Vertical"),
            "Tiles",
        );
    }
}

fn capitalize(kind: &str) -> String {
    let mut chars = kind.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The inverse of [`register_add_actions`] for `ShellView::dispatch`:
/// `(kind, explicit direction)` for a `tile::add_*` id, `None` for
/// anything else. The suffix is peeled BEFORE the kind is read, so a
/// kind can never be misparsed by a suffix of its own name, and an empty
/// kind is not an add.
pub fn parse_add_action(id: &str) -> Option<(&str, Option<crate::tiling::Orientation>)> {
    use crate::tiling::Orientation;
    let rest = id.strip_prefix("tile::add_")?;
    let (kind, dir) = if let Some(k) = rest.strip_suffix("_horizontal") {
        (k, Some(Orientation::Horizontal))
    } else if let Some(k) = rest.strip_suffix("_vertical") {
        (k, Some(Orientation::Vertical))
    } else {
        (rest, None)
    };
    (!kind.is_empty()).then_some((kind, dir))
}

/// The default primary modifier (spec §3.1: Alt, remappable).
pub fn default_mod() -> Modifiers {
    Modifiers::ALT
}

/// Resolve the `mod` alias from config: doc `app`, key `keymap.mod`.
/// `"ctrl"` is refused with an error diagnostic and the default alias is
/// returned — ctrl is reserved for the shipped literal bindings
/// (`ctrl+1..9`, `ctrl+0`, `ctrl+k`, `ctrl+/` …), and aliasing `mod` onto
/// it makes every `mod` chord collide with one of them (Phase 4a user
/// ruling, Task 4b: rather than document the collisions, refuse the
/// alias). Any other unknown value keeps today's behaviour: it silently
/// falls back to the default, with no diagnostic.
pub fn mod_alias_from_config(config: &Config) -> (Modifiers, Vec<Diagnostic>) {
    match config.get("app", "keymap.mod").and_then(|v| v.as_str()) {
        Some("ctrl") => (
            default_mod(),
            vec![Diagnostic {
                severity: Severity::Error,
                layer: config.explain("app", "keymap.mod"),
                file: None,
                message: "app: keymap.mod = \"ctrl\" is not allowed — ctrl is reserved for the \
                          shipped literal bindings (ctrl+1..9, ctrl+0, ctrl+k, ctrl+/ …); \
                          use \"alt\" or \"cmd\""
                    .into(),
                path: None,
            }],
        ),
        Some("cmd") => (Modifiers::CMD, Vec::new()),
        Some("alt") => (Modifiers::ALT, Vec::new()),
        _ => (default_mod(), Vec::new()),
    }
}

/// `[app] modules.default` is no longer read (spec 2026-09-08 add-tile
/// §7.1): tiles are added by kind. A layer that still sets it gets one
/// warning so the key does not silently rot in a desk file.
pub fn modules_default_diagnostic(config: &Config) -> Option<Diagnostic> {
    config.get("app", "modules.default").map(|_| Diagnostic {
        severity: Severity::Warning,
        layer: config.explain("app", "modules.default"),
        file: None,
        message: "app: modules.default is no longer read — tiles are added by kind \
                  (ctrl+k → Add …); remove the key"
            .into(),
        path: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{KeyContext, MatchResult, Matcher, build_keymap, parse_keystroke};
    use geode_core::config::{ConfigSources, LayerDoc};

    /// Resolve a single keystroke spec (e.g. `"ctrl+1"`) against
    /// `BUILTIN_KEYMAP` built with `mod_alias`, in the always-active
    /// `workspace` context — a pure, no-window rerun of exactly what
    /// `Matcher::press` does on a real keypress.
    fn resolve(mod_alias: Modifiers, spec: &str) -> MatchResult {
        let mut reg = ActionRegistry::default();
        register_builtin_actions(&mut reg);
        let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
        let (keymap, diags) = build_keymap(&[doc], mod_alias, &reg);
        assert!(diags.is_empty(), "{diags:?}");
        let keystroke = parse_keystroke(spec, mod_alias).unwrap();
        Matcher::default().press(&keymap, keystroke, &[KeyContext::new("workspace")])
    }

    /// A `Config` with only a builtin `app` doc holding `text` — the
    /// minimal fixture `mod_alias_from_config`'s own tests need.
    fn config_from_app(text: &str) -> Config {
        Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", text).unwrap()],
            desk: None,
            user: None,
        })
    }

    #[test]
    fn builtin_keymap_builds_clean_against_builtin_actions() {
        let mut reg = ActionRegistry::default();
        register_builtin_actions(&mut reg);
        let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
        let (keymap, diags) = build_keymap(&[doc], default_mod(), &reg);
        assert!(
            diags.is_empty(),
            "builtin keymap must be diagnostic-free: {diags:?}"
        );
        // 4 focus + 4 move + 4 resize + orientation toggle + fullscreen
        // + close + 2 duplicates + 3 dock toggles + 3 dock moves + 9
        // workspace switches + 2 palette::toggle bindings + theme toggle
        // + settings::open (Task 5) + 2 font size steps + perf overlay
        // toggle (spec §7.4). No splits: the add-tile task retired them
        // and freed ctrl+v/ctrl+h (spec 2026-09-08 add-tile §3.1).
        assert!(keymap.bindings().len() >= 39);
    }

    #[test]
    fn dock_actions_are_registered_with_the_dock_category() {
        use crate::actions::ActionId;
        let mut reg = ActionRegistry::default();
        register_builtin_actions(&mut reg);
        for id in [
            "dock::toggle_left",
            "dock::toggle_right",
            "dock::toggle_bottom",
            "dock::move_left",
            "dock::move_right",
            "dock::move_bottom",
        ] {
            let def = reg
                .get(&ActionId(id.to_string()))
                .unwrap_or_else(|| panic!("{id} not registered"));
            assert_eq!(def.category, "Dock", "{id}");
        }
    }

    #[test]
    fn mod_alias_read_from_config_with_fallback() {
        let empty = Config::load(&ConfigSources::default());
        let (alias, diags) = mod_alias_from_config(&empty);
        assert_eq!(alias, default_mod());
        assert!(diags.is_empty(), "{diags:?}");

        let bogus = config_from_app("[keymap]\nmod = \"hyper\"\n");
        let (alias, diags) = mod_alias_from_config(&bogus);
        assert_eq!(alias, default_mod());
        assert!(diags.is_empty(), "{diags:?}");
    }

    /// Task 4b (Phase 4a, user ruling): `keymap.mod = "ctrl"` is refused
    /// as invalid config rather than merely documented as a collision
    /// hazard — see `mod_alias_from_config`'s own doc comment.
    #[test]
    fn mod_alias_ctrl_is_refused_with_an_error_and_the_default_stands() {
        let config = config_from_app("[keymap]\nmod = \"ctrl\"\n");
        let (alias, diags) = mod_alias_from_config(&config);
        assert_eq!(alias, default_mod());
        let d = diags
            .iter()
            .find(|d| d.message.contains("keymap.mod"))
            .unwrap();
        assert_eq!(d.severity, Severity::Error);
        assert!(d.message.contains("ctrl"), "{}", d.message);
    }

    #[test]
    fn mod_alias_alt_and_cmd_are_accepted_without_diagnostics() {
        for (value, expect_ctrl) in [("alt", false), ("cmd", false)] {
            let (alias, diags) =
                mod_alias_from_config(&config_from_app(&format!("[keymap]\nmod = \"{value}\"\n")));
            assert!(diags.is_empty(), "{value}: {diags:?}");
            assert_eq!(alias.ctrl, expect_ctrl);
        }
    }

    #[test]
    fn modules_default_in_config_is_a_warning_and_absent_is_silent() {
        let none = Config::load(&ConfigSources::default());
        assert!(modules_default_diagnostic(&none).is_none());
        let set = Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", "[modules]\ndefault = \"blotter\"\n").unwrap()],
            desk: None,
            user: None,
        });
        let diag = modules_default_diagnostic(&set).expect("a diagnostic");
        assert_eq!(diag.severity, Severity::Warning);
        assert!(diag.message.contains("modules.default"), "{}", diag.message);
        assert!(diag.message.contains("Add"), "{}", diag.message);
    }

    /// Fix round 1 (review Finding 1): if `mod_alias` were ever
    /// `Modifiers::CTRL`, `mod+1` would parse to the exact same keystroke
    /// as `ctrl+1` — so BUILTIN_KEYMAP's shipped `workspace::switch_1`
    /// must still win that tie. It does because the frame-slot table is
    /// declared *before* the `workspace` table (see `BUILTIN_KEYMAP`'s
    /// own doc comment) and the matcher keeps the last declaration-order
    /// match. `mod_alias_from_config` itself now refuses to ever produce
    /// `Modifiers::CTRL` from config (Task 4b), so this test exercises
    /// the matcher's tie-break directly with the raw `Modifiers` value —
    /// a regression guard kept for defense in depth, independent of
    /// config validation.
    #[test]
    fn ctrl_1_resolves_to_the_shipped_workspace_switch_under_a_ctrl_mod_alias() {
        match resolve(Modifiers::CTRL, "ctrl+1") {
            MatchResult::Matched { action, .. } => {
                assert_eq!(action.0, "workspace::switch_1");
            }
            other => panic!("expected a match, got {other:?}"),
        }
    }

    /// The other half of Finding 1's fix: under the default mod alias
    /// (Alt), `mod+1` parses to `alt+1` — no collision with `ctrl+1` — so
    /// the frame slot binding resolves normally.
    #[test]
    fn ctrl_1_resolves_to_the_frame_slot_under_the_default_mod_alias() {
        match resolve(default_mod(), "ctrl+1") {
            MatchResult::Matched { action, .. } => {
                assert_eq!(action.0, "frame::slot_1");
            }
            other => panic!("expected a match, got {other:?}"),
        }
    }

    /// `BLOTTER_ACTIONS` and `BLOTTER_ACTION_DEFS` are two separate
    /// consts in this file (a plain id list for the cross-crate mirror
    /// test, a (id, title) list to register with) — this guards them
    /// against drifting apart from each other, since `geode-blotter`'s
    /// own mirror test only ever sees `BLOTTER_ACTIONS`.
    #[test]
    fn the_two_blotter_action_lists_in_this_file_agree() {
        let ids: Vec<&str> = BLOTTER_ACTION_DEFS.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, BLOTTER_ACTIONS.to_vec());
    }

    /// Every `blotter::*` id the builtin keymap binds (Step 5) must be
    /// registered here — this is what makes
    /// `builtin_keymap_builds_clean_against_builtin_actions` diagnostic-
    /// free once the blotter bindings are appended.
    #[test]
    fn every_blotter_binding_target_is_reserved() {
        let mut reg = ActionRegistry::default();
        register_builtin_actions(&mut reg);
        for id in BLOTTER_ACTIONS {
            assert!(
                reg.contains(&ActionId((*id).to_string())),
                "{id} must be reserved by register_builtin_actions"
            );
        }
    }

    /// Same drift guard as [`the_two_blotter_action_lists_in_this_file_agree`],
    /// for the diagnostics pair.
    #[test]
    fn the_two_diagnostics_action_lists_in_this_file_agree() {
        let ids: Vec<&str> = DIAGNOSTICS_ACTION_DEFS.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, DIAGNOSTICS_ACTIONS.to_vec());
    }

    /// Every `diagnostics::*` id the builtin keymap binds (the
    /// `diagnostics` context block) must be registered here — same
    /// reasoning as [`every_blotter_binding_target_is_reserved`].
    #[test]
    fn every_diagnostics_binding_target_is_reserved() {
        let mut reg = ActionRegistry::default();
        register_builtin_actions(&mut reg);
        for id in DIAGNOSTICS_ACTIONS {
            assert!(
                reg.contains(&ActionId((*id).to_string())),
                "{id} must be reserved by register_builtin_actions"
            );
        }
    }

    /// Spec 2026-09-08 add-tile §3.1: the split verbs are retired and
    /// their chords are free — tiles are added by kind instead.
    #[test]
    fn the_split_actions_are_gone() {
        use crate::actions::ActionId;
        let mut reg = ActionRegistry::default();
        register_builtin_actions(&mut reg);
        assert!(!reg.contains(&ActionId("workspace::split_right".into())));
        assert!(!reg.contains(&ActionId("workspace::split_down".into())));
        assert!(!BUILTIN_KEYMAP.contains("\"ctrl+v\""));
        assert!(!BUILTIN_KEYMAP.contains("\"ctrl+h\""));
    }

    #[test]
    fn register_add_actions_registers_three_rows_per_kind_in_the_tiles_category() {
        let mut reg = ActionRegistry::default();
        register_add_actions(&mut reg, &["blotter", "diagnostics"]);
        let expect = |id: &str, title: &str| {
            let def = reg
                .iter()
                .find(|d| d.id.0 == id)
                .unwrap_or_else(|| panic!("{id} not registered"));
            assert_eq!(def.title, title);
            assert_eq!(def.category, "Tiles");
        };
        expect("tile::add_blotter", "Blotter: Split");
        expect("tile::add_blotter_horizontal", "Blotter: Split Horizontal");
        expect("tile::add_blotter_vertical", "Blotter: Split Vertical");
        expect("tile::add_diagnostics", "Diagnostics: Split");
        assert_eq!(reg.iter().count(), 6);
        let mut empty = ActionRegistry::default();
        register_add_actions(&mut empty, &[]);
        assert_eq!(empty.iter().count(), 0);
    }

    #[test]
    fn parse_add_action_peels_the_direction_suffix_before_the_kind() {
        use crate::tiling::Orientation;
        assert_eq!(
            parse_add_action("tile::add_blotter"),
            Some(("blotter", None))
        );
        assert_eq!(
            parse_add_action("tile::add_blotter_horizontal"),
            Some(("blotter", Some(Orientation::Horizontal)))
        );
        assert_eq!(
            parse_add_action("tile::add_blotter_vertical"),
            Some(("blotter", Some(Orientation::Vertical)))
        );
        assert_eq!(parse_add_action("tile::add_"), None);
        assert_eq!(parse_add_action("tile::add__vertical"), None);
        assert_eq!(parse_add_action("tile::command_line"), None);
        assert_eq!(parse_add_action("workspace::close_tile"), None);
    }

    #[test]
    fn duplicate_actions_are_registered_and_bound() {
        use crate::actions::ActionId;
        let mut reg = ActionRegistry::default();
        register_builtin_actions(&mut reg);
        assert!(reg.contains(&ActionId("workspace::duplicate_horizontal".into())));
        assert!(reg.contains(&ActionId("workspace::duplicate_vertical".into())));
        let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
        let (keymap, diags) = build_keymap(&[doc], default_mod(), &reg);
        assert!(diags.is_empty(), "{diags:?}");
        let bound: Vec<String> = keymap
            .bindings()
            .iter()
            .map(|b| b.action.0.clone())
            .collect();
        assert!(bound.iter().any(|a| a == "workspace::duplicate_horizontal"));
        assert!(bound.iter().any(|a| a == "workspace::duplicate_vertical"));
    }
}
