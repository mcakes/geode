//! Compiled-in shell actions and keymap defaults.
//! Desk and user bindings override these through the ordinary keymap compiler.

use crate::actions::{ActionDef, ActionId, ActionRegistry};
use crate::keymap::Modifiers;
use geode_core::config::{Config, Diagnostic, Severity};

/// Shell builtin bindings, followed by module fragments and desk/user layers.
/// Module-context bindings belong to module factories' default keymap fragments.
///
/// Focus uses `mod+h/j/k/l`; move-tile uses `ctrl+alt+arrows`; resize uses
/// `shift+arrows`. Resize moves the adjacent divider toward the arrow, including
/// [`crate::tiling::Tree::move_divider`]'s edge-flip behavior. Control-arrow focus
/// would conflict with macOS Mission Control/Spaces shortcuts.
///
/// Dock moves use `ctrl+{`, `ctrl+}`, and `ctrl+?`: the pinned macOS and Windows
/// backends deliver shifted punctuation as the shifted character with Shift
/// cleared. Letters and arrows retain an explicit Shift modifier instead.
///
/// The builtin document's only multi-key sequence is the grid motion `g g`;
/// module fragments add their own `g` sequences beside it (a pending `g` waits
/// for either). Exact ties use the last matching entry. Frame-slot bindings
/// precede workspace bindings, although the configured `mod` alias cannot be
/// Control and therefore cannot collide with the literal Control digit bindings.
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
"mod+s" = "stack::split"
"mod+shift+h" = "stack::pull_left"
"mod+shift+j" = "stack::pull_down"
"mod+shift+k" = "stack::pull_up"
"mod+shift+l" = "stack::pull_right"
"mod+f" = "workspace::fullscreen_tile"
"ctrl+w" = "workspace::close_tile"
"shift+d" = "workspace::duplicate_horizontal"
"ctrl+shift+d" = "workspace::duplicate_vertical"
"ctrl+[" = "dock::toggle_left"
"ctrl+]" = "dock::toggle_right"
"ctrl+/" = "dock::toggle_bottom"
"mod+]" = "stack::next"
"mod+[" = "stack::prev"
"ctrl+{" = "dock::move_left"
"ctrl+}" = "dock::move_right"
"ctrl+?" = "dock::move_bottom"

# Context-free: a workspace switch is application navigation, reachable
# from a page (whose context stack carries no `workspace`) as well as
# from the tile surface.
[[bindings]]
[bindings.keys]
"mod+1" = "workspace::switch_1"
"mod+2" = "workspace::switch_2"
"mod+3" = "workspace::switch_3"
"mod+4" = "workspace::switch_4"
"mod+5" = "workspace::switch_5"
"mod+6" = "workspace::switch_6"
"mod+7" = "workspace::switch_7"
"mod+8" = "workspace::switch_8"
"mod+9" = "workspace::switch_9"
"ctrl+k" = "palette::toggle"
"ctrl+shift+p" = "palette::toggle"
"ctrl+," = "settings::open"
"ctrl+=" = "fontsize::increase"
"ctrl+-" = "fontsize::decrease"
"mod+shift+p" = "perf::toggle_overlay"
"mod+z" = "frame::scope_undo"
"mod+shift+z" = "frame::scope_redo"
"mod+/" = "frame::focus_text"
"mod+p" = "frame::pick"
"mod+x" = "frame::add_expression"
"mod+t" = "frame::as_of"
"mod+g" = "frame::grouping"
"mod+u" = "tile::link_group"
"mod+o" = "frame::scope"
"mod+n" = "tile::add"

[[bindings]]
context = "tile"
[bindings.keys]
":" = "tile::command_line"
"/" = "tile::find"

[[bindings]]
context = "page"
[bindings.keys]
"escape" = "page::close"

# Grid motions, shipped once for every tile that publishes `grid`. Named
# keys come first and vim keys last in the same context, so a hint and the
# keybindings dialog name the vim key (the last live binding wins display).
[[bindings]]
context = "grid && (mode == normal || mode == visual)"
[bindings.keys]
"down" = "motion::down"
"up" = "motion::up"
"left" = "motion::left"
"right" = "motion::right"
"pagedown" = "motion::page_down"
"pageup" = "motion::page_up"
"home" = "motion::line_start"
"end" = "motion::line_end"

[[bindings]]
context = "grid && (mode == normal || mode == visual)"
[bindings.keys]
"j" = "motion::down"
"k" = "motion::up"
"h" = "motion::left"
"l" = "motion::right"
"g g" = "motion::top"
"shift+g" = "motion::bottom"
"ctrl+d" = "motion::half_page_down"
"ctrl+u" = "motion::half_page_up"
"ctrl+f" = "motion::page_down"
"ctrl+b" = "motion::page_up"
"^" = "motion::line_start"
"$" = "motion::line_end"

# Menu and popup-list steps, for a tile publishing `tilelist` while one is
# open; a grid under the menu reports `mode == menu`, so its motions stay out.
[[bindings]]
context = "tilelist"
[bindings.keys]
"down" = "motion::menu_down"
"up" = "motion::menu_up"

[[bindings]]
context = "tilelist"
[bindings.keys]
"j" = "motion::menu_down"
"k" = "motion::menu_up"
"#;

/// The one context every grid motion is bound under. One string for both
/// modes, because the keybindings dialog rebinds inside the displayed
/// binding's context: two contexts would leave the other mode on the old key.
pub const GRID_MOTION_CONTEXT: &str = "grid && (mode == normal || mode == visual)";

/// The shared motion vocabulary, registered here so one binding reaches every
/// tile that publishes `grid` or `tilelist`. The shell handles none of them:
/// dispatch falls through to the focused tile, which interprets them through
/// `geode_tile::motion`.
pub const MOTION_ACTIONS: &[(&str, &str)] = &[
    ("motion::down", "Cursor down"),
    ("motion::up", "Cursor up"),
    ("motion::left", "Cursor left"),
    ("motion::right", "Cursor right"),
    ("motion::top", "Cursor to top"),
    ("motion::bottom", "Cursor to bottom"),
    ("motion::half_page_down", "Half page down"),
    ("motion::half_page_up", "Half page up"),
    ("motion::page_down", "Page down"),
    ("motion::page_up", "Page up"),
    ("motion::line_start", "First column"),
    ("motion::line_end", "Last column"),
    ("motion::menu_down", "Menu: next"),
    ("motion::menu_up", "Menu: previous"),
];

/// The context a shared motion ships under, `None` for any other action.
/// The keybindings dialog writes a Motion row's edits here rather than into
/// the displayed binding's context, which an old per-module override can
/// make a module context: an edit of a shared motion is global.
pub fn shared_motion_context(action: &ActionId) -> Option<&'static str> {
    let id = action.0.as_str();
    if !MOTION_ACTIONS.iter().any(|(m, _)| *m == id) {
        None
    } else if matches!(id, "motion::menu_down" | "motion::menu_up") {
        Some(crate::keymap::TILELIST)
    } else {
        Some(GRID_MOTION_CONTEXT)
    }
}

fn action(reg: &mut ActionRegistry, id: &str, title: &str, category: &str) {
    reg.register(ActionDef {
        id: ActionId(id.to_string()),
        title: title.to_string(),
        category: category.to_string(),
    })
    .expect("builtin action ids are unique by construction");
}

/// Register shell-owned actions. Factories register module-owned actions separately.
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
    // Reorient the pair around the focused tile through Tree::toggle_split_orientation.
    action(
        reg,
        "workspace::toggle_split_orientation",
        "Toggle split orientation",
        "Workspace",
    );
    // Duplicate the focused tile with its serialized state: horizontal places
    // it to the right, vertical places it below.
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
    // A page replaces the tile surface while open; `escape` in context `page`
    // closes it unless the page consumes the close itself.
    action(reg, "page::close", "Close page", "Workspace");
    // Dock toggles retain hidden occupants. Move sends the focused tile to a
    // dock, or back to the tree when that dock is already focused.
    action(reg, "dock::toggle_left", "Toggle left dock", "Dock");
    action(reg, "dock::toggle_right", "Toggle right dock", "Dock");
    action(reg, "dock::toggle_bottom", "Toggle bottom dock", "Dock");
    action(reg, "dock::move_left", "Move tile to left dock", "Dock");
    action(reg, "dock::move_right", "Move tile to right dock", "Dock");
    action(reg, "dock::move_bottom", "Move tile to bottom dock", "Dock");
    // Cycle stack members, pick a member, or unstack it; pick/unstack are palette-only.
    action(reg, "stack::next", "Stack: Next", "Workspace");
    action(reg, "stack::prev", "Stack: Previous", "Workspace");
    action(reg, "stack::pick", "Stack: Pick…", "Workspace");
    action(reg, "stack::unstack", "Stack: Unstack", "Workspace");
    // Split a whole stack back into tiles, or pull the visible neighbour in a
    // direction into the focused tile's slot. ShellView dispatches both: the
    // split's orientation needs geometry and a refused pull leaves a notice.
    action(reg, "stack::split", "Stack: Split into tiles", "Workspace");
    action(
        reg,
        "stack::pull_left",
        "Stack: Pull from left",
        "Workspace",
    );
    action(
        reg,
        "stack::pull_down",
        "Stack: Pull from below",
        "Workspace",
    );
    action(reg, "stack::pull_up", "Stack: Pull from above", "Workspace");
    action(
        reg,
        "stack::pull_right",
        "Stack: Pull from right",
        "Workspace",
    );
    for i in 1..=9 {
        action(
            reg,
            &format!("workspace::switch_{i}"),
            &format!("Switch to workspace {i}"),
            "Workspace",
        );
    }
    action(reg, "palette::toggle", "Toggle command palette", "Palette");
    // Settings share the Appearance category with themes. A theme name
    // selects its light/dark presentation; there is no separate mode action.
    action(reg, "settings::open", "Open settings…", "Appearance");
    // Font size steps clamp to the supported range. `ctrl+=` uses the
    // unshifted key; literal `+` is the keystroke parser's separator.
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
    // Cycle `[ui] line_numbers` through off, on, and relative; palette-only.
    action(
        reg,
        "ui::line_numbers_cycle",
        "Cycle line numbers (off / on / relative)",
        "Appearance",
    );
    // Open the keybinding editor from the palette, under Keyboard.
    action(reg, "keybindings::open", "Keyboard shortcuts…", "Keyboard");
    // The palette groups named-object editors under Configuration.
    action(reg, "config::views", "Edit views…", "Configuration");
    // Edit the groupings assigned to the nine Control-digit slots.
    action(reg, "config::groupings", "Edit groupings…", "Configuration");
    // Manage saved scopes; their separate `scope::<name>` actions recall them.
    action(reg, "config::scopes", "Edit scopes…", "Configuration");
    // Inspect the read-only dataset schema used by configuration choices.
    action(reg, "config::schema", "Edit schema…", "Configuration");
    // Open Views or Schema on one column of the focused tile's view: a list of
    // its columns, the cursor's highlighted. Palette-only.
    action(
        reg,
        "config::view_column",
        "Edit column in view…",
        "Configuration",
    );
    action(
        reg,
        "config::schema_column",
        "Edit column in schema…",
        "Configuration",
    );
    // Edit ingest sources, grouped by dataset.
    action(reg, "config::sources", "Edit sources…", "Configuration");
    // Edit named colors shared by column presentation and chart series.
    action(reg, "config::colors", "Edit colors…", "Configuration");
    reg.register_rename("config::colours", "config::colors")
        .expect("a retired id is never a registered one");
    // Edit named scope expressions, which saved scopes and the frame tick.
    action(
        reg,
        "config::expressions",
        "Edit expressions…",
        "Configuration",
    );
    // Open the user configuration directory in the OS file manager for edits
    // outside the dialogs. The title omits an ellipsis because this opens a folder.
    action(
        reg,
        "config::open_directory",
        "Open config directory",
        "Configuration",
    );
    // Toggle the frame-time overlay with `mod+shift+p`.
    // Counter reset is available through the palette.
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
    // Command and find prompts are shell actions in the occupied `tile`
    // context, so every module gets `:` and `/` without registering them.
    action(
        reg,
        "tile::command_line",
        "Open the tile command line",
        "Tile",
    );
    action(reg, "tile::find", "Find in tile", "Tile");
    // The tile picker lists roster kinds. Its id is outside the `tile::add_`
    // prefix so parse_add_action cannot mistake it for a specific module kind.
    action(reg, "tile::add", "Add a tile…", "Tiles");
    // Pulls the focused tile's dimension context and lists the kinds that
    // accept one of its columns. Outside the `tile::add_` prefix, like
    // `tile::add`.
    action(reg, "tile::open_with", "Open with context…", "Tiles");
    // The row menu on the focused tile's cursor row: the kinds and
    // dimension actions its single-valued columns offer.
    action(reg, "tile::context_menu", "Row actions\u{2026}", "Tiles");
    // Fits the focused tile's table columns to their content
    // (`TileContent::autosize_columns`). Palette-only: no default key.
    action(reg, "tile::autosize_columns", "Autosize columns", "Tile");
    // The link chooser on the focused tile: the group it follows and, for
    // a tile whose module emits, the group it emits into.
    action(reg, "tile::link_group", "Link group\u{2026}", "Tile");
    // Control-1 through Control-9 activate grouping slots; empty slots are
    // ignored. Control-0 restores each following tile's view-default grouping.
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
    // Scope undo/redo traverse Frame's bounded history. Clearing the whole
    // scope is a separate palette action.
    action(reg, "frame::scope_undo", "Undo scope change", "Frame");
    action(reg, "frame::scope_redo", "Redo scope change", "Frame");
    action(reg, "frame::scope_clear", "Clear scope", "Frame");
    // Give the active workspace its own scope, grouping, and as-of, or
    // return it to the shared frame (discarding its own). Toolbar glyph
    // and palette; no default chord.
    action(
        reg,
        "frame::pin_workspace",
        "Toggle the frame pin for this workspace",
        "Frame",
    );
    // Save the current frame scope through the Scopes dialog naming prompt.
    // Dispatch this id before matching the generic `scope::` prefix; otherwise
    // `save_current` would be interpreted as a scope name. That name is reserved
    // in scope configuration for the same reason.
    action(
        reg,
        "scope::save_current",
        "Scope: Save current as…",
        "Scope",
    );
    // Open the dimension picker at column selection. Per-column actions are
    // registered separately from the startup schema by register_pick_actions.
    action(reg, "frame::pick", "Pick a dimension…", "Frame");
    // The grouping picker lists filled slots and the view default with typeahead.
    action(reg, "frame::grouping", "Pick a grouping…", "Frame");
    // The scope picker lists the frame's live saved scopes with typeahead;
    // a pick loads one undoably, like the `scope::<name>` actions.
    action(reg, "frame::scope", "Pick a scope…", "Frame");
    // Focus the scope text field; its Input handles typing once focused.
    action(
        reg,
        "frame::focus_text",
        "Focus the scope text field",
        "Frame",
    );
    // Open the as-of selector with recent generation timestamps as presets.
    // Live mode and as-of undo are separate palette actions.
    action(reg, "frame::as_of", "Jump to a point in time…", "Frame");
    action(reg, "frame::live", "Return to live", "Frame");
    action(
        reg,
        "frame::as_of_undo",
        "Swap to the previous as of",
        "Frame",
    );
    // Open the frame-scope expression dialog through the palette.
    action(
        reg,
        "frame::scope_expression",
        "Set scope expression…",
        "Frame",
    );
    // Join a new expression to the current one with `and` (the toolbar's
    // add-a-filter menu reaches the same action; `mod+x`), and drop the
    // whole expression layer undoably (no default chord).
    action(
        reg,
        "frame::add_expression",
        "Add scope expression…",
        "Frame",
    );
    action(
        reg,
        "frame::clear_expression",
        "Clear scope expression",
        "Frame",
    );
    // Choose a log target and level, then submit Diagnostics::request_level.
    action(reg, "log::level", "Set log level…", "Diagnostics");
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

    // The shared motions: one registration, one "Motion" group in the
    // palette and the keybindings dialog, whichever tile takes them.
    for (id, title) in MOTION_ACTIONS {
        action(reg, id, title, "Motion");
    }
}

/// Register `frame::pick_<column>` actions from the startup pickable columns.
/// Registration precedes keymap compilation so these ids are bindable. The action
/// registry is fixed at startup; newly pickable columns need restart for new ids.
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

/// Register palette and keymap actions `scope::<name>` for startup saved scopes.
/// They are unbound by default. Reload updates saved scope contents, but newly
/// added names require restart to enter the action registry.
pub fn register_scope_actions(reg: &mut ActionRegistry, saved: &geode_core::scopes::SavedScopes) {
    for name in saved.keys() {
        let id = format!("scope::{name}");
        // A hand-written scope name may collide with a shell action. Keep the
        // existing action and skip this palette row rather than panic at startup.
        if reg.contains(&ActionId(id.clone())) {
            tracing::warn!(
                target: "geode::config",
                "scopes: '{name}' collides with an existing action id ({id}); no palette row for it"
            );
            continue;
        }
        action(reg, &id, &format!("Scope: {name}"), "Scope");
    }
}

/// A kind's add-tile id suffixes and palette titles: default-direction
/// split, explicit horizontal/right split, explicit vertical/below split,
/// and stack. The non-empty suffixes are `PLACEMENT_SUFFIXES`, which the
/// panel reader refuses at the end of a panel name.
const ADD_PLACEMENTS: [(&str, &str); 4] = [
    ("", "Split"),
    ("_horizontal", "Split Horizontal"),
    ("_vertical", "Split Vertical"),
    ("_stacked", "Stack"),
];

/// Register four Tiles actions per module kind, ids `tile::add_<kind>`
/// with optional `_horizontal`, `_vertical`, or `_stacked` suffixes.
/// Registration precedes keymap compilation.
///
/// Kinds come from configuration (market-data panels) as well as code, so
/// uniqueness is not by construction. A kind ending in a placement suffix
/// (its default id would decode as another kind), or one whose ids another
/// registration already holds, gets no add-tile actions and an Error
/// naming it, returned for the caller's diagnostics; the startup does not
/// panic. The kind's rows are refused whole, so its palette never offers
/// some placements and not others.
pub fn register_add_actions(reg: &mut ActionRegistry, kinds: &[&str]) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    for kind in kinds {
        let ids: Vec<String> = ADD_PLACEMENTS
            .iter()
            .map(|(suffix, _)| format!("tile::add_{kind}{suffix}"))
            .collect();
        let refusal = if let Some(suffix) = geode_core::panel::PLACEMENT_SUFFIXES
            .iter()
            .find(|s| kind.ends_with(*s))
        {
            Some(format!(
                "ends in the reserved placement suffix '{suffix}', so 'tile::add_{kind}' would decode as another kind"
            ))
        } else {
            ids.iter()
                .find(|id| reg.contains(&ActionId((*id).clone())))
                .map(|id| format!("'{id}' is already registered"))
        };
        if let Some(why) = refusal {
            diags.push(Diagnostic {
                severity: Severity::Error,
                layer: None,
                file: None,
                message: format!("tile kind '{kind}' {why}; it has no add-tile actions"),
                path: None,
            });
            continue;
        }
        let title = capitalize(kind);
        for (id, (_, placement)) in ids.iter().zip(ADD_PLACEMENTS) {
            action(reg, id, &format!("{title}: {placement}"), "Tiles");
        }
    }
    diags
}

/// Register one toggle per page kind, mirroring `register_add_actions`:
/// `page::toggle_<kind>` titled "<Title>: Open page" in category `<Title>`.
pub fn register_page_actions(reg: &mut ActionRegistry, pages: &[(&str, &str)]) {
    for (kind, title) in pages {
        action(
            reg,
            &format!("page::toggle_{kind}"),
            &format!("{title}: Open page"),
            title,
        );
    }
}

/// A kind's palette title (`blotter` → `Blotter`) — also the tile
/// picker's row text (`shell::choicedialog`), so the two spell a kind
/// the same way.
pub fn capitalize(kind: &str) -> String {
    let mut chars = kind.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Place a tile in an explicit/default-direction split or the focused stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddPlacement {
    Split(Option<crate::tiling::Orientation>),
    Stacked,
}

/// Decode a `tile::add_*` action into kind and placement; reject empty kinds.
/// Placement suffixes are stripped first, so kind names ending in a reserved
/// suffix are ambiguous and cannot be represented as a default-direction add.
pub fn parse_add_action(id: &str) -> Option<(&str, AddPlacement)> {
    use crate::tiling::Orientation;
    let rest = id.strip_prefix("tile::add_")?;
    let (kind, placement) = if let Some(k) = rest.strip_suffix("_horizontal") {
        (k, AddPlacement::Split(Some(Orientation::Horizontal)))
    } else if let Some(k) = rest.strip_suffix("_vertical") {
        (k, AddPlacement::Split(Some(Orientation::Vertical)))
    } else if let Some(k) = rest.strip_suffix("_stacked") {
        (k, AddPlacement::Stacked)
    } else {
        (rest, AddPlacement::Split(None))
    };
    (!kind.is_empty()).then_some((kind, placement))
}

/// Default primary modifier: Alt.
pub fn default_mod() -> Modifiers {
    Modifiers::ALT
}

/// Resolve `app.toml`'s `[keymap] mod`: `alt` or `cmd`.
/// `ctrl` returns an error and the Alt fallback because it collides with shipped
/// literal Control bindings. Missing, non-string, and other string values silently
/// fall back to Alt. Values are case-sensitive.
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

/// Warn when the ignored `app.toml` key `modules.default` is present.
/// Tiles are created by explicit module kind.
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
        // The builtin document contains the complete shell binding set.
        assert!(keymap.bindings().len() >= 39);
    }

    /// A user binding to the old `config::colours` id still opens the Colors
    /// dialog: it binds `config::colors`, with a warning naming both ids.
    #[test]
    fn a_user_binding_to_the_old_colours_action_binds_config_colors() {
        let mut reg = ActionRegistry::default();
        register_builtin_actions(&mut reg);
        let user = LayerDoc {
            layer: geode_core::config::Layer::User,
            name: "keymap".into(),
            file: "user/keymap.toml".into(),
            table: "[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"mod+shift+k\" = \"config::colours\"\n"
                .parse()
                .unwrap(),
        };
        let (keymap, diags) = build_keymap(&[user], default_mod(), &reg);
        assert_eq!(keymap.bindings().len(), 1, "{diags:?}");
        assert_eq!(keymap.bindings()[0].action.0, "config::colors");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("config::colours"), "{diags:?}");
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

    /// Opening the configuration folder is a Configuration action without an ellipsis.
    #[test]
    fn open_config_directory_is_registered_under_configuration_without_ellipsis() {
        use crate::actions::ActionId;
        let mut reg = ActionRegistry::default();
        register_builtin_actions(&mut reg);
        let def = reg
            .get(&ActionId("config::open_directory".to_string()))
            .expect("config::open_directory registered");
        assert_eq!(def.category, "Configuration");
        assert_eq!(def.title, "Open config directory");
    }

    /// A configured scope cannot replace a shell action with the same id.
    /// Other scopes still register after a collision is skipped.
    #[test]
    fn register_scope_actions_skips_a_collision_and_does_not_panic() {
        use crate::actions::ActionId;
        let mut reg = ActionRegistry::default();
        register_builtin_actions(&mut reg);
        let mut saved = geode_core::scopes::SavedScopes::new();
        saved.insert(
            "save_current".to_string(),
            geode_core::scope::Scope::default(),
        );
        saved.insert("eu".to_string(), geode_core::scope::Scope::default());

        register_scope_actions(&mut reg, &saved); // must not panic

        let builtin = reg
            .get(&ActionId("scope::save_current".to_string()))
            .expect("the builtin action must still be registered");
        assert_eq!(
            builtin.title, "Scope: Save current as…",
            "the collision must not overwrite the builtin action's own title"
        );
        assert!(
            reg.contains(&ActionId("scope::eu".to_string())),
            "a saved scope with no colliding id still gets its own row"
        );
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

    /// Control is refused as the primary modifier to protect literal Control bindings.
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

    /// A raw Control alias bypasses config validation. Workspace switching
    /// still wins the digit tie because its entry follows the frame-slot entry.
    #[test]
    fn ctrl_1_resolves_to_the_shipped_workspace_switch_under_a_ctrl_mod_alias() {
        match resolve(Modifiers::CTRL, "ctrl+1") {
            MatchResult::Matched { action, .. } => {
                assert_eq!(action.0, "workspace::switch_1");
            }
            other => panic!("expected a match, got {other:?}"),
        }
    }

    /// With the default Alt alias, Control-digit frame slots have no alias collision.
    #[test]
    fn ctrl_1_resolves_to_the_frame_slot_under_the_default_mod_alias() {
        match resolve(default_mod(), "ctrl+1") {
            MatchResult::Matched { action, .. } => {
                assert_eq!(action.0, "frame::slot_1");
            }
            other => panic!("expected a match, got {other:?}"),
        }
    }

    /// Generic split actions are absent; tile additions identify the module kind.
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
    fn register_add_actions_registers_four_rows_per_kind_in_the_tiles_category() {
        let mut reg = ActionRegistry::default();
        register_add_actions(&mut reg, &["blotter", "pricer"]);
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
        expect("tile::add_blotter_stacked", "Blotter: Stack");
        expect("tile::add_pricer", "Pricer: Split");
        assert_eq!(reg.iter().count(), 8);
        let mut empty = ActionRegistry::default();
        register_add_actions(&mut empty, &[]);
        assert_eq!(empty.iter().count(), 0);
    }

    /// The panel reader refuses a name ending in `PLACEMENT_SUFFIXES`; the
    /// shell's own suffixes must be exactly that list, and each must decode
    /// back to its kind.
    #[test]
    fn the_add_placement_suffixes_are_the_ones_the_panel_reader_reserves() {
        let suffixes: Vec<&str> = ADD_PLACEMENTS
            .iter()
            .map(|(s, _)| *s)
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(suffixes, geode_core::panel::PLACEMENT_SUFFIXES);
        for suffix in suffixes {
            let id = format!("tile::add_k{suffix}");
            let (kind, placement) = parse_add_action(&id).unwrap();
            assert_eq!(kind, "k", "{suffix}");
            assert_ne!(placement, AddPlacement::Split(None), "{suffix}");
        }
    }

    /// Kinds come from configuration too: a kind whose ids collide with
    /// another kind's placement ids, or that ends in a placement suffix, is
    /// refused with an Error instead of panicking the startup.
    #[test]
    fn register_add_actions_refuses_a_colliding_kind_without_panicking() {
        let mut reg = ActionRegistry::default();
        let diags = register_add_actions(&mut reg, &["cvi", "cvi_stacked", "vol_vertical", "ok"]);
        assert_eq!(diags.len(), 2, "{diags:?}");
        assert!(diags.iter().all(|d| d.severity == Severity::Error));
        assert!(
            diags[0].message.contains("'cvi_stacked'"),
            "{}",
            diags[0].message
        );
        assert!(
            diags[1].message.contains("'vol_vertical'"),
            "{}",
            diags[1].message
        );
        assert_eq!(
            reg.get(&ActionId("tile::add_cvi_stacked".into()))
                .unwrap()
                .title,
            "Cvi: Stack",
            "the first kind keeps its stack row"
        );
        assert!(!reg.contains(&ActionId("tile::add_cvi_stacked_vertical".into())));
        assert!(reg.contains(&ActionId("tile::add_ok_stacked".into())));
        // Four rows each for `cvi` and `ok`.
        assert_eq!(reg.iter().count(), 8);
        // An id held by an earlier registration refuses the kind whole.
        let mut reg = ActionRegistry::default();
        reg.register(ActionDef {
            id: ActionId("tile::add_rec_vertical".into()),
            title: "Taken".into(),
            category: "Test".into(),
        })
        .unwrap();
        let diags = register_add_actions(&mut reg, &["rec"]);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(
            diags[0]
                .message
                .contains("'tile::add_rec_vertical' is already registered"),
            "{}",
            diags[0].message
        );
        assert!(!reg.contains(&ActionId("tile::add_rec".into())));
    }

    #[test]
    fn page_actions_register_one_toggle_per_kind() {
        let mut reg = ActionRegistry::default();
        register_page_actions(&mut reg, &[("diagnostics", "Diagnostics")]);
        let def = reg
            .get(&ActionId("page::toggle_diagnostics".into()))
            .expect("registered");
        assert_eq!(def.title, "Diagnostics: Open page");
        assert_eq!(def.category, "Diagnostics");
        assert_eq!(reg.iter().count(), 1);
    }

    #[test]
    fn parse_add_action_peels_the_direction_suffix_before_the_kind() {
        use crate::tiling::Orientation;
        assert_eq!(
            parse_add_action("tile::add_blotter"),
            Some(("blotter", AddPlacement::Split(None)))
        );
        assert_eq!(
            parse_add_action("tile::add_blotter_horizontal"),
            Some((
                "blotter",
                AddPlacement::Split(Some(Orientation::Horizontal))
            ))
        );
        assert_eq!(
            parse_add_action("tile::add_blotter_vertical"),
            Some(("blotter", AddPlacement::Split(Some(Orientation::Vertical))))
        );
        assert_eq!(
            parse_add_action("tile::add_blotter_stacked"),
            Some(("blotter", AddPlacement::Stacked))
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

    /// `BUILTIN_KEYMAP` plus an optional user `keymap.toml`, over the builtin
    /// registry, as startup compiles them.
    fn motion_keymap(user: Option<&str>) -> (crate::keymap::Keymap, Vec<Diagnostic>) {
        let mut reg = ActionRegistry::default();
        register_builtin_actions(&mut reg);
        let mut docs = vec![LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap()];
        if let Some(text) = user {
            docs.push(LayerDoc {
                layer: geode_core::config::Layer::User,
                name: "keymap".into(),
                file: "user/keymap.toml".into(),
                table: text.parse().unwrap(),
            });
        }
        build_keymap(&docs, default_mod(), &reg)
    }

    /// Feed a space-separated key sequence to one matcher; the last answer.
    fn press_all(keymap: &crate::keymap::Keymap, keys: &str, stack: &[KeyContext]) -> MatchResult {
        let mut matcher = Matcher::default();
        let mut last = MatchResult::NoMatch;
        for key in keys.split(' ') {
            last = matcher.press(keymap, parse_keystroke(key, default_mod()).unwrap(), stack);
        }
        last
    }

    fn grid_stack(mode: &str) -> Vec<KeyContext> {
        vec![
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("rec").grid().pair("mode", mode).counts(),
        ]
    }

    fn matched(action: &str, count: Option<u32>) -> MatchResult {
        MatchResult::Matched {
            action: ActionId(action.to_string()),
            count,
        }
    }

    #[test]
    fn the_grid_motion_context_constant_is_the_shipped_one() {
        assert!(BUILTIN_KEYMAP.contains(&format!("context = \"{GRID_MOTION_CONTEXT}\"")));
    }

    #[test]
    fn grid_motions_resolve_in_normal_and_visual_grid_contexts_only() {
        let (keymap, diags) = motion_keymap(None);
        assert!(diags.is_empty(), "{diags:?}");
        for mode in ["normal", "visual"] {
            let stack = grid_stack(mode);
            for (keys, action, count) in [
                ("j", "motion::down", None),
                ("down", "motion::down", None),
                ("3 j", "motion::down", Some(3)),
                ("k", "motion::up", None),
                ("up", "motion::up", None),
                ("h", "motion::left", None),
                ("left", "motion::left", None),
                ("l", "motion::right", None),
                ("right", "motion::right", None),
                ("g g", "motion::top", None),
                ("5 shift+g", "motion::bottom", Some(5)),
                ("ctrl+d", "motion::half_page_down", None),
                ("ctrl+u", "motion::half_page_up", None),
                ("ctrl+f", "motion::page_down", None),
                ("pagedown", "motion::page_down", None),
                ("ctrl+b", "motion::page_up", None),
                ("pageup", "motion::page_up", None),
                ("^", "motion::line_start", None),
                ("home", "motion::line_start", None),
                ("$", "motion::line_end", None),
                ("end", "motion::line_end", None),
            ] {
                assert_eq!(
                    press_all(&keymap, keys, &stack),
                    matched(action, count),
                    "{mode}: {keys}"
                );
            }
        }
        // Insert keeps typing; a tile without the flag is untouched.
        let timeseries = vec![
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("timeseries")
                .pair("mode", "normal")
                .counts(),
        ];
        for stack in [grid_stack("insert"), grid_stack("menu"), timeseries] {
            assert_eq!(
                press_all(&keymap, "j", &stack),
                MatchResult::NoMatch,
                "{stack:?}"
            );
        }
    }

    #[test]
    fn an_open_tile_list_takes_j_and_k_as_menu_steps_not_grid_motions() {
        let (keymap, diags) = motion_keymap(None);
        assert!(diags.is_empty(), "{diags:?}");
        let menu_over_grid = vec![
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("rec")
                .grid()
                .tilelist()
                .pair("mode", "menu")
                .counts(),
        ];
        let list_without_grid = vec![
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("timeseries")
                .tilelist()
                .pair("mode", "normal")
                .counts(),
        ];
        for stack in [menu_over_grid, list_without_grid] {
            for (key, action) in [
                ("j", "motion::menu_down"),
                ("down", "motion::menu_down"),
                ("k", "motion::menu_up"),
                ("up", "motion::menu_up"),
            ] {
                assert_eq!(
                    press_all(&keymap, key, &stack),
                    matched(action, None),
                    "{key}"
                );
            }
        }
    }

    /// One user entry under the shipped context rebinds both modes, and a
    /// `"none"` there unbinds the shipped key in every grid tile.
    #[test]
    fn one_user_override_under_the_shipped_context_rebinds_both_modes() {
        let user = format!(
            "[[bindings]]\ncontext = \"{GRID_MOTION_CONTEXT}\"\n[bindings.keys]\n\
             \"n\" = \"motion::down\"\n\"j\" = \"none\"\n"
        );
        let (keymap, diags) = motion_keymap(Some(&user));
        assert!(diags.is_empty(), "{diags:?}");
        for mode in ["normal", "visual"] {
            let stack = grid_stack(mode);
            assert_eq!(
                press_all(&keymap, "n", &stack),
                matched("motion::down", None),
                "{mode}"
            );
            assert_eq!(
                press_all(&keymap, "j", &stack),
                MatchResult::NoMatch,
                "{mode}"
            );
            assert_eq!(
                press_all(&keymap, "down", &stack),
                matched("motion::down", None),
                "{mode}"
            );
        }
    }

    /// A user entry naming a retired module id binds the shared id, only in
    /// the context it was written for, with a warning naming both ids.
    #[test]
    fn a_retired_module_motion_binds_the_shared_id_in_its_own_context() {
        let mut reg = ActionRegistry::default();
        register_builtin_actions(&mut reg);
        reg.register_rename("rec::down", "motion::down").unwrap();
        let user = LayerDoc {
            layer: geode_core::config::Layer::User,
            name: "keymap".into(),
            file: "user/keymap.toml".into(),
            table: "[[bindings]]\ncontext = \"rec && mode == normal\"\n[bindings.keys]\n\"q\" = \"rec::down\"\n"
                .parse()
                .unwrap(),
        };
        let builtin = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
        let (keymap, diags) = build_keymap(&[builtin, user], default_mod(), &reg);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].severity, Severity::Warning);
        assert!(
            diags[0].message.contains("rec::down") && diags[0].message.contains("motion::down"),
            "{}",
            diags[0].message
        );
        assert_eq!(
            press_all(&keymap, "q", &grid_stack("normal")),
            matched("motion::down", None)
        );
        let other = vec![
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("other")
                .grid()
                .pair("mode", "normal")
                .counts(),
        ];
        assert_eq!(press_all(&keymap, "q", &other), MatchResult::NoMatch);
    }
}
