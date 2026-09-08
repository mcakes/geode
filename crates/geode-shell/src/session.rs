//! Session layout persistence (Task 3): saves/restores the workspace
//! tiling layout across app restarts. Pure layout state — theme choices
//! persist separately, into the user config layer (see `theme::
//! persist_to_user_config`); this file no longer carries a `theme_mode`
//! (removed: theme changes now persist via `toml_edit` into `app.toml`
//! instead of the session file, so they survive the ordinary desk/user
//! config merge like any other config value, not a side channel).
//!
//! State-as-config (brief): the session file lives alongside desk/user
//! config, at `user_config_dir()/session.toml` (wired in `geode-app`'s
//! `main.rs`), and is declarative, hand-editable TOML — same shape as
//! every other config document in this codebase, not a binary blob.
//!
//! Format choice: manual `toml::Table`/`toml::Value` construction, matching
//! how `geode-core::config` already hand-builds and reads TOML elsewhere in
//! this codebase (no serde derive) — `serde`/`serde_json` are already in the
//! dependency graph (see `theme.rs`), but a derive-based `Deserialize`
//! either panics/rejects on the exact hostile shapes this module must heal
//! (a dangling `focused` id, drifted ratios) or requires as much custom
//! `Deserialize` code as the manual version anyway, for a node encoding
//! that's recursive and enum-tagged. Manual construction keeps one error
//! path (`Result<_, Vec<String>>`) instead of two (serde's error type plus
//! this module's own healing logic).
//!
//! Node encoding (nested tables mirroring [`Node`]):
//! ```toml
//! config_version = 1
//! active = 1
//!
//! [workspaces.1]
//! focused = 2
//! # fullscreen = 3   # present only when a tile is fullscreen
//!
//! [workspaces.1.node]
//! kind = "split"
//! orientation = "horizontal"
//! ratios = [0.5, 0.5]
//!
//! [[workspaces.1.node.children]]
//! kind = "leaf"
//! id = 1
//!
//! [[workspaces.1.node.children]]
//! kind = "leaf"
//! id = 2
//! ```
//!
//! Dock-regions task adds two optional per-workspace shapes (absent in
//! every pre-dock file, which therefore loads unchanged — no
//! `config_version` bump; `from_toml` tolerates unknown keys in both
//! directions): a `region` string (`"left"`/`"right"`/`"bottom"`; absent
//! or `"main"` = focus in the main tree — only written when focus lives in
//! a dock) and `[workspaces.N.docks.left/right/bottom]` tables. The
//! dock-trees task made each dock a full tiling tree, so a dock table now
//! carries the *same* recursive node encoding a workspace does — an
//! optional `focused` (int) plus an optional `[….node]` subtree (both via
//! the shared `node_to_toml`/`node_from_toml`/`Tree::from_parts` seams) —
//! alongside `visible` (bool) and `size` (float 0.10..=0.50); never a
//! `fullscreen` (dock trees can't have one; a hostile file's claim is
//! ignored with a warning). Only docks that differ from the default
//! (hidden, empty, default size) are written at all.
//!
//! Legacy dock shape: the first dock-regions build (one tile per dock)
//! wrote `tile = N` instead of a node subtree. That key still loads —
//! healed into a single-leaf tree, silently (recorded choice: it's the
//! expected output of the immediately-prior release, not corruption, so no
//! warning; a file carrying BOTH `tile` and `node` picks the node and does
//! warn, since no release ever wrote that shape).
//!
//! Dock corruption heals with a warning instead of failing the workspace
//! (the docks are an adornment on the layout, never worth discarding the
//! main tree over): a structurally invalid dock node drops that dock's
//! tree, a duplicate tile claim (already in a main tree or an earlier dock
//! tree, this workspace or any other) is removed from the dock's tree, a
//! `region` pointing at a hidden/empty dock falls back to `Main`, and an
//! out-of-range/NaN `size` resets to the default — see
//! `Workspace::from_parts` / `Workspaces::from_parts` for the cross-tree
//! healing seams themselves.
//!
//! Old session files written before this removal may still carry an
//! `[extra]` table with a `theme_mode` key; `from_toml` never reads
//! `extra` at all now, so it is simply ignored like any other unknown
//! top-level key — the layout underneath it still loads.
//!
//! Every workspace present in [`Workspaces::spaces`] is written, empty ones
//! included (mirrors the in-memory behavior: "workspaces materialize lazily
//! on first switch and persist" — spec §3.6), so a session restore leaves
//! the *set* of touched workspaces exactly as the user left it, not just the
//! non-empty ones.

use std::collections::BTreeMap;
use std::path::Path;

use crate::tiling::{
    DOCK_MAX_SIZE, DOCK_MIN_SIZE, Dock, DockSide, Docks, FocusRegion, Node, Orientation, TileId,
    Tree, Workspace, Workspaces,
};
use geode_core::query::AsOf;
use geode_core::scope::{DimensionSelection, Scope, parse_expr};

/// Schema version written into every session file, and enforced on load
/// exactly like `geode_core::config::load_layer` enforces `config_version`
/// for desk/user config docs: present and matching → fine; present and
/// different → the whole session is invalid (an `Err`, naming the version
/// found — `from_toml`'s caller, `load`, treats that as "fresh start,
/// warn"); missing entirely → a warning, but the rest of the file still
/// gets parsed (mirrors `load_layer` treating a missing version as
/// "assume current" rather than a hard failure).
pub const SESSION_CONFIG_VERSION: i64 = 1;

/// A tile's restored occupant kind and opaque module state (Phase 3 §3.5).
/// Formalised here in Task 3 with `restored_tiles` always empty; Task 4
/// fills it in from each workspace's `tiles` table and
/// `ShellView::ensure_occupants` consumes it as tiles get their occupants.
/// `state` is whatever the module's `serialize` returned; the shell never
/// reads inside it.
///
/// `kind` is stored exactly as written in the file — `from_toml` has no
/// module roster to check it against, so a `kind` from an unregistered or
/// downgraded module round-trips here unchanged. Resolution happens one
/// layer up, in `occupants::ensure_occupants`: when the roster has no
/// factory for `kind`, it falls back to the default factory with `state`
/// discarded (a factory must never see state shaped for a different
/// module), so the occupant it creates is really the *default* kind with
/// empty state. `to_toml`'s flush then serializes whatever the live
/// occupant reports (`current_tiles`), so that healed default silently
/// overwrites the original unknown-kind record on the very next save —
/// acceptable healing (M15, 3b final review), but real state loss for a
/// misconfigured or downgraded run, worth knowing rather than discovering.
#[derive(Debug, Clone, PartialEq)]
pub struct TileRecord {
    pub kind: String,
    pub state: toml::Table,
}

/// Every tile's record, keyed by the raw `TileId` (`u64`) it belongs to —
/// `BTreeMap` for deterministic iteration (matters for `to_toml`'s written
/// key order, and for tests comparing round-tripped maps).
pub type TileRecords = BTreeMap<u64, TileRecord>;

/// What [`load`]/[`from_toml`] hand back: the restored layout, each tile's
/// module record, the frame's own restored state if the file had one, and
/// any non-fatal warnings accumulated healing any of it.
#[derive(Debug)]
pub struct Restored {
    pub workspaces: Workspaces,
    pub tiles: TileRecords,
    pub frame: Option<FrameRecord>,
    pub warnings: Vec<String>,
}

/// The frame's own restored state (Phase 4a §3.6, §3.12): the global
/// scope, the active grouping slot, and as-of. Written under session.toml's
/// `[frame]` table by [`to_toml`] when a caller passes one, and read back
/// by [`from_toml`] into [`Restored::frame`] — `ShellView::new` applies it
/// to the just-built `Frame` and then clears the undo entry that push
/// leaves behind (`Frame::clear_history`), so a restored session doesn't
/// start with a phantom "undo" back to the empty scope nobody chose.
///
/// Deliberately not the whole `Frame`: undo/redo history, recent
/// publishes, and saved scopes are either transient (nothing to undo back
/// to yet) or already config (`scopes.toml`) — restoring them here would
/// either mean nothing on a fresh process or duplicate what the config
/// layer already owns.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameRecord {
    pub scope: Scope,
    pub active_slot: Option<u8>,
    pub as_of: AsOf,
}

impl FrameRecord {
    /// The `[frame]` table `to_toml` writes — present-only-when-meaningful
    /// like every other optional field in this file (`slot`/`as_of` are
    /// omitted rather than written as their empty/live default).
    pub fn to_toml(&self) -> toml::Table {
        let mut t = toml::Table::new();
        let mut dims = toml::Table::new();
        for d in self
            .scope
            .dimensions
            .iter()
            .filter(|d| !d.values.is_empty())
        {
            dims.insert(
                d.column.clone(),
                toml::Value::Array(
                    d.values
                        .iter()
                        .map(|v| toml::Value::String(v.clone()))
                        .collect(),
                ),
            );
        }
        t.insert("dimensions".into(), toml::Value::Table(dims));
        if let Some(text) = &self.scope.text {
            t.insert("text".into(), toml::Value::String(text.clone()));
        }
        if let Some(e) = &self.scope.expression {
            t.insert("expression".into(), toml::Value::String(e.to_string()));
        }
        if let Some(n) = self.active_slot {
            t.insert("slot".into(), toml::Value::Integer(n as i64));
        }
        if let AsOf::At(at) = &self.as_of {
            t.insert("as_of".into(), toml::Value::String(at.to_rfc3339()));
        }
        t
    }

    /// Lenient: a field that does not parse is dropped with a warning, the
    /// rest of the record survives — same philosophy as the tiles/docks
    /// healing elsewhere in this file. No schema/dataset validation here
    /// (unlike `geode_core::scopes::saved_scopes_from_doc`): this layer
    /// has no schema to check against, so a dangling column name simply
    /// round-trips through unchanged, the same way a `TileRecord`'s
    /// unrecognised `kind` does.
    pub fn from_toml(t: &toml::Table, warnings: &mut Vec<String>) -> FrameRecord {
        let mut dimensions = Vec::new();
        if let Some(dims_table) = t.get("dimensions").and_then(|v| v.as_table()) {
            for (column, values) in dims_table {
                let Some(arr) = values.as_array() else {
                    warnings.push(format!(
                        "frame: dimension '{column}' is not an array; ignored"
                    ));
                    continue;
                };
                let values: Vec<String> = arr
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(str::to_string)
                    .collect();
                if !values.is_empty() {
                    dimensions.push(DimensionSelection {
                        column: column.clone(),
                        values,
                    });
                }
            }
        }
        let text = t
            .get("text")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let expression = t
            .get("expression")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .and_then(|s| match parse_expr(s) {
                Ok(e) => Some(e),
                Err(e) => {
                    warnings.push(format!("frame: expression '{s}': {e}; dropped"));
                    None
                }
            });
        let active_slot = match t.get("slot") {
            None => None,
            Some(v) => match v.as_integer() {
                Some(n) if (1..=9).contains(&n) => Some(n as u8),
                other => {
                    warnings.push(format!("frame: slot {other:?} is not 1-9; ignored"));
                    None
                }
            },
        };
        let as_of = match t.get("as_of").and_then(|v| v.as_str()) {
            None => AsOf::Live,
            Some(s) => match chrono::DateTime::parse_from_rfc3339(s) {
                Ok(dt) => AsOf::At(dt.with_timezone(&chrono::Utc)),
                Err(e) => {
                    warnings.push(format!("frame: as_of '{s}': {e}; using live"));
                    AsOf::Live
                }
            },
        };
        FrameRecord {
            scope: Scope {
                dimensions,
                text,
                expression,
                impossible: false,
            },
            active_slot,
            as_of,
        }
    }
}

/// Serialize `workspaces` and each tile's `tiles` record into a session
/// TOML table (pure, no I/O — see [`save`] for the file-writing wrapper).
/// `tiles` may hold a record for an id belonging to any workspace; only
/// records whose id actually lives in the workspace being written land
/// under that workspace's `tiles` table — a stale entry (a tile since
/// closed) is silently dropped, never written to a workspace it doesn't
/// belong to. `frame` writes a top-level `[frame]` table (Phase 4a §3.6)
/// when `Some`, and is omitted entirely (not written as an empty table)
/// when `None` — every pre-4a session file keeps loading unchanged.
pub fn to_toml(
    workspaces: &Workspaces,
    tiles: &TileRecords,
    frame: Option<&FrameRecord>,
) -> toml::Table {
    let mut root = toml::Table::new();
    root.insert(
        "config_version".to_string(),
        toml::Value::Integer(SESSION_CONFIG_VERSION),
    );
    root.insert(
        "active".to_string(),
        toml::Value::Integer(i64::from(workspaces.active_index())),
    );

    let mut spaces_table = toml::Table::new();
    for (ix, workspace) in workspaces.spaces() {
        let tree = workspace.tree();
        let mut ws_table = toml::Table::new();
        if let Some(node) = tree.root() {
            ws_table.insert("node".to_string(), node_to_toml(node));
        }
        if let Some(focused) = tree.focused() {
            ws_table.insert(
                "focused".to_string(),
                toml::Value::Integer(tile_id_to_i64(focused)),
            );
        }
        if let Some(fullscreen) = tree.fullscreen() {
            ws_table.insert(
                "fullscreen".to_string(),
                toml::Value::Integer(tile_id_to_i64(fullscreen)),
            );
        }
        // Dock-regions task. `region` is written only when focus actually
        // lives in a dock (mirrors `focused`/`fullscreen`'s present-only-
        // when-meaningful style; absent = "main"), and a dock table only
        // when the dock differs from its default (hidden, empty, default
        // size) — so a pre-dock-shaped session keeps writing byte-for-byte
        // the same file it always did.
        if let FocusRegion::Dock(side) = workspace.region() {
            ws_table.insert(
                "region".to_string(),
                toml::Value::String(region_side_name(side).to_string()),
            );
        }
        let mut docks_table = toml::Table::new();
        for (side, dock) in workspace.docks().iter() {
            if *dock == Dock::default() {
                continue;
            }
            let mut dock_table = toml::Table::new();
            // Same recursive encoding as the workspace's own tree
            // (dock-trees task); no `fullscreen` — dock trees never have
            // one (see `Dock::from_parts`).
            if let Some(node) = dock.tree().root() {
                dock_table.insert("node".to_string(), node_to_toml(node));
            }
            if let Some(focused) = dock.tree().focused() {
                dock_table.insert(
                    "focused".to_string(),
                    toml::Value::Integer(tile_id_to_i64(focused)),
                );
            }
            dock_table.insert("visible".to_string(), toml::Value::Boolean(dock.visible()));
            dock_table.insert(
                "size".to_string(),
                toml::Value::Float(f64::from(dock.size())),
            );
            docks_table.insert(
                region_side_name(side).to_string(),
                toml::Value::Table(dock_table),
            );
        }
        if !docks_table.is_empty() {
            ws_table.insert("docks".to_string(), toml::Value::Table(docks_table));
        }

        // Task 4 (Phase 3 §3.5): a `tiles` table per workspace, keyed by
        // tile id, module kind plus opaque state. Only ids that actually
        // belong to this workspace (main tree or any dock) are considered
        // — a record for an id that isn't here (a closed tile whose
        // `current_tiles` snapshot hasn't been refreshed yet, or a hand-
        // edited file) is simply not written; `from_toml` applies the same
        // membership check symmetrically on the way back in. This writes
        // whatever `tiles` currently holds for each id with no notion of
        // where that record came from — see [`TileRecord`]'s own doc
        // comment: if `id`'s original record named a kind the roster
        // healed away on load, `tiles` by now holds the healed default
        // kind with empty state instead, and that is what lands here.
        let mut tiles_table = toml::Table::new();
        let mut here: Vec<TileId> = tree.tiles();
        for (_, dock) in workspace.docks().iter() {
            here.extend(dock.tree().tiles());
        }
        for id in here {
            let Some(record) = tiles.get(&id.0) else {
                continue;
            };
            let mut t = toml::Table::new();
            t.insert(
                "module".to_string(),
                toml::Value::String(record.kind.clone()),
            );
            if !record.state.is_empty() {
                t.insert(
                    "state".to_string(),
                    toml::Value::Table(record.state.clone()),
                );
            }
            tiles_table.insert(id.0.to_string(), toml::Value::Table(t));
        }
        if !tiles_table.is_empty() {
            ws_table.insert("tiles".to_string(), toml::Value::Table(tiles_table));
        }

        spaces_table.insert(ix.to_string(), toml::Value::Table(ws_table));
    }
    root.insert("workspaces".to_string(), toml::Value::Table(spaces_table));

    if let Some(record) = frame {
        root.insert("frame".to_string(), toml::Value::Table(record.to_toml()));
    }

    root
}

/// Deserialize a session TOML table back into a [`Restored`] (pure, no
/// I/O). Tolerant of unknown keys (only the fields documented at the top of
/// this file are ever read — notably including a legacy `[extra]` table,
/// e.g. a `theme_mode` key written by a build before theme changes moved to
/// the user config layer: it is simply never looked at, so the layout
/// underneath it still loads). Any structural corruption — a mismatched
/// `config_version` (see [`SESSION_CONFIG_VERSION`]), a `Split` with a bad
/// arity/ratio-length mismatch or a non-finite/non-positive ratio (see
/// [`Tree::from_parts`]), an unparseable node, an out-of-range `active` —
/// collects into the `Err` variant rather than partially applying;
/// [`load`] treats that as "fresh start, warn". A dangling
/// `focused`/`fullscreen` reference is healed silently by
/// `Tree::from_parts`, not an error; a missing `config_version` is a
/// warning that still lets the rest of the file parse.
///
/// A `tiles` entry (Phase 3 §3.5, Task 4) is dropped, with a warning, when
/// its key isn't a valid id, its id isn't a tile in that workspace's
/// layout (main tree or any dock — a dangling record, e.g. from a tile
/// closed since the file was written), it isn't a table, it has no
/// `module`, or its `state` is present but not a table — never itself a
/// reason to fail the whole session.
pub fn from_toml(table: &toml::Table) -> Result<Restored, Vec<String>> {
    let mut warnings = Vec::new();
    match table.get("config_version") {
        Some(toml::Value::Integer(v)) if *v == SESSION_CONFIG_VERSION => {}
        Some(other) => {
            return Err(vec![format!(
                "unsupported session config_version {other} (this build supports {SESSION_CONFIG_VERSION})"
            )]);
        }
        None => {
            warnings.push(format!(
                "missing config_version (assuming {SESSION_CONFIG_VERSION})"
            ));
        }
    }

    let mut errors = Vec::new();

    let active = match table.get("active") {
        None => 1u8,
        Some(value) => match value.as_integer() {
            Some(n) if (1..=9).contains(&n) => n as u8,
            Some(n) => {
                errors.push(format!("active workspace {n} is out of range 1..=9"));
                1
            }
            None => {
                errors.push("active is not an integer".to_string());
                1
            }
        },
    };

    let mut spaces = BTreeMap::new();
    let mut tiles = TileRecords::new();
    if let Some(workspaces_value) = table.get("workspaces") {
        match workspaces_value.as_table() {
            Some(workspaces_table) => {
                for (key, value) in workspaces_table {
                    match parse_workspace(key, value, &mut warnings, &mut tiles) {
                        Ok((ix, workspace)) => {
                            spaces.insert(ix, workspace);
                        }
                        Err(e) => errors.push(e),
                    }
                }
            }
            None => errors.push("workspaces is not a table".to_string()),
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    let (workspaces, heal_warnings) =
        Workspaces::from_parts(spaces, active).map_err(|e| vec![e])?;
    warnings.extend(heal_warnings);

    // Phase 4a §3.6: the frame's own restored state, if the file had one.
    // Absent entirely (every pre-4a file) means "nothing to restore", not
    // an error; present but not a table is a warning, same tolerance the
    // rest of this function extends to every other optional shape.
    let frame = match table.get("frame") {
        None => None,
        Some(toml::Value::Table(t)) => Some(FrameRecord::from_toml(t, &mut warnings)),
        Some(_) => {
            warnings.push("frame is not a table; ignored".to_string());
            None
        }
    };

    Ok(Restored {
        workspaces,
        tiles,
        frame,
        warnings,
    })
}

fn parse_workspace(
    key: &str,
    value: &toml::Value,
    warnings: &mut Vec<String>,
    out_tiles: &mut TileRecords,
) -> Result<(u8, Workspace), String> {
    let ix: u8 = key
        .parse()
        .map_err(|_| format!("workspace key '{key}' is not a valid index"))?;
    if !(1..=9).contains(&ix) {
        return Err(format!("workspace index {ix} is out of range 1..=9"));
    }
    let ws_table = value
        .as_table()
        .ok_or_else(|| format!("workspace {ix} is not a table"))?;

    let root = match ws_table.get("node") {
        Some(node_value) => {
            Some(node_from_toml(node_value).map_err(|e| format!("workspace {ix}: {e}"))?)
        }
        None => None,
    };
    let focused = ws_table
        .get("focused")
        .and_then(|v| v.as_integer())
        .map(|v| TileId(v.max(0) as u64));
    let fullscreen = ws_table
        .get("fullscreen")
        .and_then(|v| v.as_integer())
        .map(|v| TileId(v.max(0) as u64));

    let tree =
        Tree::from_parts(root, focused, fullscreen).map_err(|e| format!("workspace {ix}: {e}"))?;

    // Dock-regions task: docks and region are strictly optional — absent
    // means "no docks, focus in Main", which is exactly what every pre-dock
    // session file deserializes to. Everything hostile inside them heals
    // with a warning rather than failing the workspace: the docks are an
    // adornment on the layout, never worth discarding the tree over.
    let docks = parse_docks(ix, ws_table.get("docks"), warnings);
    let region = parse_region(ix, ws_table.get("region"), warnings);

    // Per-workspace healing (duplicate claims against this tree, an
    // unfocusable region) lives in `Workspace::from_parts`; the
    // cross-workspace pass runs later in `Workspaces::from_parts`.
    let (workspace, heal_warnings) = Workspace::from_parts(tree, docks, region);
    warnings.extend(
        heal_warnings
            .into_iter()
            .map(|w| format!("workspace {ix}: {w}")),
    );

    // Task 4 (Phase 3 §3.5): each tile's module record. Membership is
    // checked against the *healed* workspace (main tree plus every dock),
    // not the raw ids this function started from — a duplicate/dangling
    // claim `Workspace::from_parts` already dropped above must not still
    // accept a tile record keyed to it.
    if let Some(tiles_value) = ws_table.get("tiles") {
        match tiles_value.as_table() {
            None => warnings.push(format!("workspace {ix}: tiles is not a table; ignored")),
            Some(tiles_table) => {
                let mut here: Vec<u64> = workspace.tree().tiles().iter().map(|t| t.0).collect();
                for (_, dock) in workspace.docks().iter() {
                    here.extend(dock.tree().tiles().iter().map(|t| t.0));
                }
                for (key, value) in tiles_table {
                    let Ok(id) = key.parse::<u64>() else {
                        warnings.push(format!(
                            "workspace {ix}: tile key '{key}' is not an id; ignored"
                        ));
                        continue;
                    };
                    if !here.contains(&id) {
                        warnings.push(format!(
                            "workspace {ix}: tile {id} has a record but is not in the layout; ignored"
                        ));
                        continue;
                    }
                    let Some(t) = value.as_table() else {
                        warnings.push(format!("workspace {ix}: tile {id} is not a table; ignored"));
                        continue;
                    };
                    // Stored as-is, not checked against a module roster —
                    // this layer has none. See [`TileRecord`]'s doc
                    // comment for how an unrecognised `kind` is healed one
                    // layer up, and rewritten as that healed default on
                    // the next flush.
                    let Some(kind) = t.get("module").and_then(|v| v.as_str()) else {
                        warnings.push(format!("workspace {ix}: tile {id} has no module; ignored"));
                        continue;
                    };
                    let state = match t.get("state") {
                        None => toml::Table::new(),
                        Some(s) => match s.as_table() {
                            Some(s) => s.clone(),
                            None => {
                                warnings.push(format!(
                                    "workspace {ix}: tile {id} state is not a table; ignored"
                                ));
                                continue;
                            }
                        },
                    };
                    out_tiles.insert(
                        id,
                        TileRecord {
                            kind: kind.to_string(),
                            state,
                        },
                    );
                }
            }
        }
    }

    Ok((ix, workspace))
}

fn region_side_name(side: DockSide) -> &'static str {
    match side {
        DockSide::Left => "left",
        DockSide::Right => "right",
        DockSide::Bottom => "bottom",
    }
}

fn parse_region(ix: u8, value: Option<&toml::Value>, warnings: &mut Vec<String>) -> FocusRegion {
    match value.map(|v| (v, v.as_str())) {
        None => FocusRegion::Main,
        Some((_, Some("main"))) => FocusRegion::Main,
        Some((_, Some("left"))) => FocusRegion::Dock(DockSide::Left),
        Some((_, Some("right"))) => FocusRegion::Dock(DockSide::Right),
        Some((_, Some("bottom"))) => FocusRegion::Dock(DockSide::Bottom),
        Some((other, _)) => {
            warnings.push(format!(
                "workspace {ix}: unknown region {other}; assuming main"
            ));
            FocusRegion::Main
        }
    }
}

fn parse_docks(ix: u8, value: Option<&toml::Value>, warnings: &mut Vec<String>) -> Docks {
    let Some(value) = value else {
        return Docks::default();
    };
    let Some(table) = value.as_table() else {
        warnings.push(format!("workspace {ix}: docks is not a table; ignoring"));
        return Docks::default();
    };
    let mut parse_side = |side: DockSide| -> Dock {
        let Some(dock_value) = table.get(region_side_name(side)) else {
            return Dock::default();
        };
        let Some(dock_table) = dock_value.as_table() else {
            warnings.push(format!(
                "workspace {ix}: {} dock is not a table; ignoring",
                region_side_name(side)
            ));
            return Dock::default();
        };
        // The dock's tree: a recursive `node` subtree (dock-trees task),
        // or — legacy, from the first dock-regions build — a bare
        // `tile = N` healed into a single-leaf tree (silently: it's the
        // prior release's expected output, not corruption; carrying BOTH
        // shapes is corruption-adjacent, so that picks the node and warns).
        let node_value = dock_table.get("node");
        let legacy_tile = dock_table.get("tile");
        if node_value.is_some() && legacy_tile.is_some() {
            warnings.push(format!(
                "workspace {ix}: {} dock has both a node tree and a legacy tile key; \
                 using the node tree",
                region_side_name(side)
            ));
        }
        let root = match node_value {
            Some(nv) => match node_from_toml(nv) {
                Ok(node) => Some(node),
                Err(e) => {
                    warnings.push(format!(
                        "workspace {ix}: {} dock node is invalid ({e}); dropping the dock's tree",
                        region_side_name(side)
                    ));
                    None
                }
            },
            None => match legacy_tile {
                None => None,
                Some(v) => match v.as_integer() {
                    Some(n) if n >= 0 => Some(Node::Leaf(TileId(n as u64))),
                    _ => {
                        warnings.push(format!(
                            "workspace {ix}: {} dock tile {v} is not a non-negative integer; \
                             dropping it",
                            region_side_name(side)
                        ));
                        None
                    }
                },
            },
        };
        // Dock trees never have fullscreen — a hostile file's claim is
        // ignored, not honored (`Dock::from_parts` enforces the invariant
        // even if this warning path is somehow skipped).
        if dock_table.get("fullscreen").is_some() {
            warnings.push(format!(
                "workspace {ix}: {} dock claims a fullscreen tile, but dock trees \
                 cannot be fullscreen; ignoring it",
                region_side_name(side)
            ));
        }
        let focused = dock_table
            .get("focused")
            .and_then(|v| v.as_integer())
            .map(|v| TileId(v.max(0) as u64));
        let tree = match Tree::from_parts(root, focused, None) {
            Ok(tree) => tree,
            Err(e) => {
                warnings.push(format!(
                    "workspace {ix}: {} dock tree is invalid ({e}); dropping the dock's tree",
                    region_side_name(side)
                ));
                Tree::default()
            }
        };
        let visible = match dock_table.get("visible") {
            None => false,
            Some(v) => match v.as_bool() {
                Some(b) => b,
                None => {
                    warnings.push(format!(
                        "workspace {ix}: {} dock visible {v} is not a boolean; assuming hidden",
                        region_side_name(side)
                    ));
                    false
                }
            },
        };
        let size = match dock_table.get("size") {
            None => crate::tiling::DOCK_DEFAULT_SIZE,
            Some(v) => {
                let n = v.as_float().or_else(|| v.as_integer().map(|i| i as f64));
                match n {
                    Some(n)
                        if n.is_finite()
                            && (f64::from(DOCK_MIN_SIZE)..=f64::from(DOCK_MAX_SIZE))
                                .contains(&n) =>
                    {
                        n as f32
                    }
                    _ => {
                        warnings.push(format!(
                            "workspace {ix}: {} dock size {v} is out of range \
                             {DOCK_MIN_SIZE}..={DOCK_MAX_SIZE}; using the default",
                            region_side_name(side)
                        ));
                        crate::tiling::DOCK_DEFAULT_SIZE
                    }
                }
            }
        };
        Dock::from_parts(tree, visible, size)
    };
    let left = parse_side(DockSide::Left);
    let right = parse_side(DockSide::Right);
    let bottom = parse_side(DockSide::Bottom);
    Docks::from_parts(left, right, bottom)
}

fn tile_id_to_i64(id: TileId) -> i64 {
    // TileId's u64 stays comfortably under i64::MAX for any realistic
    // session (tile ids are allocated one at a time, in-process); a wrapped
    // negative value round-trips through `parse_workspace`'s `.max(0)`
    // clamp as 0, which just fails membership in `Tree::from_parts` and
    // gets healed to None rather than panicking or misbehaving.
    id.0 as i64
}

fn node_to_toml(node: &Node) -> toml::Value {
    match node {
        Node::Leaf(id) => {
            let mut t = toml::Table::new();
            t.insert("kind".to_string(), toml::Value::String("leaf".to_string()));
            t.insert("id".to_string(), toml::Value::Integer(tile_id_to_i64(*id)));
            toml::Value::Table(t)
        }
        Node::Split {
            orientation,
            children,
            ratios,
        } => {
            let mut t = toml::Table::new();
            t.insert("kind".to_string(), toml::Value::String("split".to_string()));
            t.insert(
                "orientation".to_string(),
                toml::Value::String(
                    match orientation {
                        Orientation::Horizontal => "horizontal",
                        Orientation::Vertical => "vertical",
                    }
                    .to_string(),
                ),
            );
            t.insert(
                "children".to_string(),
                toml::Value::Array(children.iter().map(node_to_toml).collect()),
            );
            t.insert(
                "ratios".to_string(),
                toml::Value::Array(
                    ratios
                        .iter()
                        .map(|r| toml::Value::Float(f64::from(*r)))
                        .collect(),
                ),
            );
            toml::Value::Table(t)
        }
    }
}

fn node_from_toml(value: &toml::Value) -> Result<Node, String> {
    let table = value.as_table().ok_or("node is not a table")?;
    match table.get("kind").and_then(|v| v.as_str()) {
        Some("leaf") => {
            let id = table
                .get("id")
                .and_then(|v| v.as_integer())
                .ok_or("leaf missing integer 'id'")?;
            if id < 0 {
                return Err(format!("leaf id {id} is negative"));
            }
            Ok(Node::Leaf(TileId(id as u64)))
        }
        Some("split") => {
            let orientation = match table.get("orientation").and_then(|v| v.as_str()) {
                Some("horizontal") => Orientation::Horizontal,
                Some("vertical") => Orientation::Vertical,
                other => return Err(format!("split has invalid orientation {other:?}")),
            };
            let children = table
                .get("children")
                .and_then(|v| v.as_array())
                .ok_or("split missing 'children' array")?
                .iter()
                .map(node_from_toml)
                .collect::<Result<Vec<_>, _>>()?;
            let ratios = table
                .get("ratios")
                .and_then(|v| v.as_array())
                .ok_or("split missing 'ratios' array")?
                .iter()
                .map(|v| {
                    v.as_float()
                        .or_else(|| v.as_integer().map(|i| i as f64))
                        .map(|f| f as f32)
                        .ok_or_else(|| "ratio is not a number".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Node::Split {
                orientation,
                children,
                ratios,
            })
        }
        other => Err(format!("unknown node kind {other:?}")),
    }
}

/// Pure serialization: `to_toml` + `toml::to_string_pretty`, wrapped into a
/// single `Result` type. Cheap — a handful of small TOML tables — which is
/// exactly why it's the half of session-saving that's safe to run
/// synchronously on the UI thread; [`write_atomic`] is the other half (the
/// actual file I/O) and must not be. This split exists for
/// `ShellView`'s coalesced dirty-flag flush (Task 3 fix round 1: see
/// `shell::mod`'s `take_dirty_session_write`), which serializes here on the
/// UI thread and hands the resulting `String` to a background executor for
/// [`write_atomic`].
pub fn to_string_pretty(
    workspaces: &Workspaces,
    tiles: &TileRecords,
    frame: Option<&FrameRecord>,
) -> Result<String, String> {
    let table = to_toml(workspaces, tiles, frame);
    toml::to_string_pretty(&table).map_err(|e| e.to_string())
}

/// Atomically write already-serialized session `text` to `path`: a temp
/// file in the same directory, `fsync`, then rename over `path` (rename is
/// atomic on the same filesystem — a crash or concurrent read never
/// observes a partial write). Creates the parent directory if it doesn't
/// exist yet.
///
/// This is real, potentially-blocking file I/O (Task 3 fix round 1: a
/// review finding on the first cut of this module, which ran this inline
/// on the UI thread once per workspace-mutating dispatch — holding e.g.
/// shift+left at OS key-repeat, ~20-30 events/sec, could then stall the render
/// thread on a slow filesystem). Callers driven by UI events must run this
/// on a background executor, never inline — see `ShellView`'s ~500ms
/// watcher-tick flush, which is the only per-dispatch path left after that
/// fix; [`save`] (a synchronous, do-everything wrapper) remains fine for
/// off-the-UI-thread callers: direct test use, and the best-effort
/// `on_app_quit` final flush, which fires at most once, at shutdown.
///
/// The atomic write itself is [`crate::config_write::write_file`]'s (Phase
/// 4c collapsed the three `write_atomic` copies into one door); this
/// function is now only the session file's *boundary* onto it. Two things
/// it keeps that the door cannot know:
///
/// * **`std::io::Result`.** Every caller of this function — `ShellView`'s
///   background flush, `save`, the session tests — already speaks
///   `io::Result`, so the door's `String` error is mapped back here rather
///   than rippling a signature change through them. Nothing inspects the
///   `ErrorKind`; the message is what reaches the `[session] warning:`
///   line.
/// * **A path, not a layer + doc name.** `session.toml` is per-machine
///   session state, deliberately *excluded* from the layered config merge
///   and from `reload::scan` (`reload::EXCLUDED_FILENAME`), so it must not
///   go through `config_write::write`'s layer-and-doc-name door — which
///   would imply it is a config document that hot-reloads like the others.
///
/// The temp filename the door derives for this path is
/// `.session.toml.{pid}-{counter}.tmp` — the same name this function used
/// to build itself, and still a non-`.toml` name so the reload watcher's
/// `*.toml` glob (Task 1c-1, `reload::scan`) never even sees it mid-write,
/// on top of `session.toml` itself already being excluded by name. See
/// `config_write`'s `TMP_COUNTER` for the race that uniqueness closes and
/// the residual rename ordering it deliberately leaves open — which for
/// this file is at worst a stale-but-valid session (e.g. an old periodic
/// flush's rename landing after the quit-time save's), exactly the
/// tradeoff this module's "best-effort, never load-bearing" design accepts
/// elsewhere (see `load`'s "never panic or block startup").
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    crate::config_write::write_file(path, text).map_err(std::io::Error::other)
}

/// Serialize and atomically write in one synchronous call — for callers
/// that don't need [`to_string_pretty`]/[`write_atomic`] split across a
/// UI/background boundary (direct test use; `ShellView`'s best-effort
/// `on_app_quit` flush, which is a one-shot at shutdown, not a per-keystroke
/// hot path).
pub fn save(
    path: &Path,
    workspaces: &Workspaces,
    tiles: &TileRecords,
    frame: Option<&FrameRecord>,
) -> std::io::Result<()> {
    let text = to_string_pretty(workspaces, tiles, frame)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    write_atomic(path, &text)
}

/// Load the session file at `path`, tolerantly: a missing file is a fresh
/// start with no warnings (first run, or the user deleted it — not an
/// error); a file that fails to read, parse, or validate is also a fresh
/// start, but with warning strings the caller should surface (main.rs
/// prints one `[session] warning:` line each, same convention as config/
/// keymap/theme diagnostics) — a bad session file must never panic or
/// block startup.
pub fn load(path: &Path) -> Restored {
    let fresh = |warnings: Vec<String>| Restored {
        workspaces: Workspaces::new(),
        tiles: TileRecords::new(),
        frame: None,
        warnings,
    };

    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return fresh(Vec::new()),
        Err(e) => return fresh(vec![format!("session file unreadable: {e}")]),
    };

    let table = match text.parse::<toml::Table>() {
        Ok(t) => t,
        Err(e) => return fresh(vec![format!("session file parse error: {e}")]),
    };

    match from_toml(&table) {
        Ok(restored) => restored,
        Err(errors) => fresh(errors),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiling::{Direction, apply_workspace_action};

    fn act(s: &str) -> crate::actions::ActionId {
        crate::actions::ActionId(s.to_string())
    }

    /// Two tiles side by side in workspace 1 — the simplest fixture with
    /// more than one tile to hang a `TileRecord` on. The first
    /// `split_right` on an empty tree only creates the first tile (nothing
    /// to split yet); the second actually splits it in two.
    fn two_tile_workspaces() -> Workspaces {
        let mut ws = Workspaces::new();
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        ws
    }

    // --- tiles (Phase 3 §3.5, Task 4) ------------------------------------

    #[test]
    fn tiles_round_trip_with_their_kind_and_opaque_state() {
        let ws = two_tile_workspaces();
        let ids = ws.active().tree().tiles();
        let mut tiles = TileRecords::new();
        let mut state = toml::Table::new();
        state.insert("view".into(), toml::Value::String("tree".into()));
        state.insert(
            "pinned".into(),
            toml::Value::Array(vec![toml::Value::String("lhu".into())]),
        );
        tiles.insert(
            ids[0].0,
            TileRecord {
                kind: "blotter".into(),
                state,
            },
        );
        tiles.insert(
            ids[1].0,
            TileRecord {
                kind: "blotter".into(),
                state: toml::Table::new(),
            },
        );

        let text = to_string_pretty(&ws, &tiles, None).unwrap();
        assert!(
            text.contains(&format!("[workspaces.1.tiles.{}]", ids[0].0)),
            "{text}"
        );
        assert!(text.contains("module = \"blotter\""), "{text}");
        assert!(text.contains("view = \"tree\""), "{text}");

        let restored = from_toml(&text.parse().unwrap()).unwrap();
        assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
        assert_eq!(restored.tiles, tiles);
        assert_eq!(restored.workspaces.active().tree().tiles(), ids);
    }

    #[test]
    fn a_tile_record_for_an_id_not_in_that_workspace_is_dropped_with_a_warning() {
        let ws = two_tile_workspaces();
        let mut tiles = TileRecords::new();
        tiles.insert(
            999,
            TileRecord {
                kind: "blotter".into(),
                state: toml::Table::new(),
            },
        );
        let mut table = to_toml(&ws, &tiles, None);
        // Force the stray record in under workspace 1 regardless of what
        // `to_toml` filtered.
        let ws_table = table["workspaces"]["1"].as_table_mut().unwrap();
        let mut stray = toml::Table::new();
        stray.insert("module".into(), toml::Value::String("blotter".into()));
        let mut tiles_table = ws_table
            .get("tiles")
            .and_then(|t| t.as_table())
            .cloned()
            .unwrap_or_default();
        tiles_table.insert("999".into(), toml::Value::Table(stray));
        ws_table.insert("tiles".into(), toml::Value::Table(tiles_table));

        let restored = from_toml(&table).unwrap();
        assert!(!restored.tiles.contains_key(&999));
        assert!(
            restored.warnings.iter().any(|w| w.contains("999")),
            "{:?}",
            restored.warnings
        );
    }

    #[test]
    fn a_tile_record_without_a_module_or_with_a_bad_state_is_dropped_with_a_warning() {
        let ws = two_tile_workspaces();
        let id = ws.active().tree().tiles()[0].0;
        let mut table = to_toml(&ws, &TileRecords::new(), None);
        let ws_table = table["workspaces"]["1"].as_table_mut().unwrap();
        let mut tiles_table = toml::Table::new();
        let mut no_module = toml::Table::new();
        no_module.insert("state".into(), toml::Value::Table(toml::Table::new()));
        tiles_table.insert(id.to_string(), toml::Value::Table(no_module));
        ws_table.insert("tiles".into(), toml::Value::Table(tiles_table));
        let restored = from_toml(&table).unwrap();
        assert!(restored.tiles.is_empty());
        assert_eq!(restored.warnings.len(), 1, "{:?}", restored.warnings);
    }

    #[test]
    fn a_session_without_tiles_still_loads_and_writes_no_tiles_table() {
        // Every pre-Phase-3 session file.
        let ws = two_tile_workspaces();
        let text = to_string_pretty(&ws, &TileRecords::new(), None).unwrap();
        assert!(!text.contains("tiles"), "{text}");
        let restored = from_toml(&text.parse().unwrap()).unwrap();
        assert!(restored.tiles.is_empty());
    }

    // --- to_toml / from_toml round-trip ---------------------------------

    #[test]
    fn round_trips_a_fresh_workspaces() {
        let ws = Workspaces::new();
        let table = to_toml(&ws, &TileRecords::new(), None);
        let Restored {
            workspaces: restored,
            warnings,
            ..
        } = from_toml(&table).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(restored.active_index(), ws.active_index());
        assert!(restored.active().is_empty());
    }

    #[test]
    fn round_trips_a_multi_workspace_layout() {
        let mut ws = Workspaces::new();
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        apply_workspace_action(&mut ws, &act("workspace::split_down"));
        ws.switch(3);
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        apply_workspace_action(&mut ws, &act("workspace::fullscreen_tile"));
        ws.switch(1);

        let table = to_toml(&ws, &TileRecords::new(), None);
        let Restored {
            workspaces: restored,
            warnings,
            ..
        } = from_toml(&table).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");

        assert_eq!(restored.active_index(), 1);

        let before: BTreeMap<u8, _> = ws
            .spaces()
            .map(|(ix, ws)| {
                (
                    ix,
                    (
                        ws.tree().layout(crate::tiling::Rect::UNIT),
                        ws.tree().fullscreen(),
                    ),
                )
            })
            .collect();
        let after: BTreeMap<u8, _> = restored
            .spaces()
            .map(|(ix, ws)| {
                (
                    ix,
                    (
                        ws.tree().layout(crate::tiling::Rect::UNIT),
                        ws.tree().fullscreen(),
                    ),
                )
            })
            .collect();
        assert_eq!(before, after, "every workspace's layout must round-trip");
    }

    #[test]
    fn from_toml_tolerates_a_legacy_extra_theme_mode_table() {
        // Old session files (written before theme changes moved to the
        // user config layer) may carry `[extra]\ntheme_mode = "..."`.
        // `from_toml` no longer reads `extra` at all — the layout must
        // still load intact, with the legacy key simply ignored like any
        // other unknown key.
        let mut table = to_toml(&Workspaces::new(), &TileRecords::new(), None);
        let mut extra_table = toml::Table::new();
        extra_table.insert(
            "theme_mode".to_string(),
            toml::Value::String("dark".to_string()),
        );
        table.insert("extra".to_string(), toml::Value::Table(extra_table));

        let Restored {
            workspaces: ws,
            warnings,
            ..
        } = from_toml(&table).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(ws.active_index(), 1);
        assert!(ws.active().is_empty());
    }

    #[test]
    fn from_toml_tolerates_unknown_keys() {
        let mut table = to_toml(&Workspaces::new(), &TileRecords::new(), None);
        table.insert(
            "some_future_field".to_string(),
            toml::Value::String("ignored".to_string()),
        );
        if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces")
            && let Some(toml::Value::Table(w1)) = ws_table.get_mut("1")
        {
            w1.insert("unknown".to_string(), toml::Value::Boolean(true));
        }
        assert!(from_toml(&table).is_ok());
    }

    // --- hostile inputs --------------------------------------------------

    #[test]
    fn from_toml_on_an_empty_table_is_a_fresh_workspace_one_with_a_missing_version_warning() {
        let Restored {
            workspaces: ws,
            warnings,
            ..
        } = from_toml(&toml::Table::new()).unwrap();
        assert_eq!(ws.active_index(), 1);
        assert!(ws.active().is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("config_version"));
    }

    #[test]
    fn from_toml_rejects_bad_ratios() {
        let mut table = toml::Table::new();
        table.insert("active".to_string(), toml::Value::Integer(1));
        let mut ws_table = toml::Table::new();
        let mut ws1 = toml::Table::new();
        let mut node = toml::Table::new();
        node.insert("kind".to_string(), toml::Value::String("split".to_string()));
        node.insert(
            "orientation".to_string(),
            toml::Value::String("horizontal".to_string()),
        );
        let mut leaf1 = toml::Table::new();
        leaf1.insert("kind".to_string(), toml::Value::String("leaf".to_string()));
        leaf1.insert("id".to_string(), toml::Value::Integer(1));
        let mut leaf2 = toml::Table::new();
        leaf2.insert("kind".to_string(), toml::Value::String("leaf".to_string()));
        leaf2.insert("id".to_string(), toml::Value::Integer(2));
        node.insert(
            "children".to_string(),
            toml::Value::Array(vec![toml::Value::Table(leaf1), toml::Value::Table(leaf2)]),
        );
        // A non-positive ratio — bad ratios, one of two hostile-ratio
        // shapes `Tree::from_parts` must reject; see
        // `from_toml_rejects_a_nan_ratio` below for the other (NaN).
        node.insert(
            "ratios".to_string(),
            toml::Value::Array(vec![toml::Value::Float(0.0), toml::Value::Float(1.0)]),
        );
        ws1.insert("node".to_string(), toml::Value::Table(node));
        ws_table.insert("1".to_string(), toml::Value::Table(ws1));
        table.insert("workspaces".to_string(), toml::Value::Table(ws_table));

        assert!(
            from_toml(&table).is_err(),
            "a non-positive ratio must fail the whole session, not silently apply"
        );
    }

    #[test]
    fn from_toml_rejects_a_nan_ratio() {
        // `toml::Value::Float(f64::NAN)` is directly constructible (the
        // `Value` enum just wraps an `f64`) — it's only the *text* literal
        // `nan` that TOML's own grammar handles specially, which is
        // irrelevant here since this builds a `Value` programmatically
        // rather than parsing TOML source.
        let mut table = toml::Table::new();
        table.insert("active".to_string(), toml::Value::Integer(1));
        let mut ws_table = toml::Table::new();
        let mut ws1 = toml::Table::new();
        let mut node = toml::Table::new();
        node.insert("kind".to_string(), toml::Value::String("split".to_string()));
        node.insert(
            "orientation".to_string(),
            toml::Value::String("horizontal".to_string()),
        );
        let mut leaf1 = toml::Table::new();
        leaf1.insert("kind".to_string(), toml::Value::String("leaf".to_string()));
        leaf1.insert("id".to_string(), toml::Value::Integer(1));
        let mut leaf2 = toml::Table::new();
        leaf2.insert("kind".to_string(), toml::Value::String("leaf".to_string()));
        leaf2.insert("id".to_string(), toml::Value::Integer(2));
        node.insert(
            "children".to_string(),
            toml::Value::Array(vec![toml::Value::Table(leaf1), toml::Value::Table(leaf2)]),
        );
        node.insert(
            "ratios".to_string(),
            toml::Value::Array(vec![toml::Value::Float(f64::NAN), toml::Value::Float(0.5)]),
        );
        ws1.insert("node".to_string(), toml::Value::Table(node));
        ws_table.insert("1".to_string(), toml::Value::Table(ws1));
        table.insert("workspaces".to_string(), toml::Value::Table(ws_table));

        assert!(
            from_toml(&table).is_err(),
            "a NaN ratio must fail the whole session, not silently apply"
        );
    }

    #[test]
    fn from_toml_heals_a_dangling_focused_reference() {
        let mut table = toml::Table::new();
        table.insert("active".to_string(), toml::Value::Integer(1));
        let mut ws_table = toml::Table::new();
        let mut ws1 = toml::Table::new();
        let mut leaf = toml::Table::new();
        leaf.insert("kind".to_string(), toml::Value::String("leaf".to_string()));
        leaf.insert("id".to_string(), toml::Value::Integer(1));
        ws1.insert("node".to_string(), toml::Value::Table(leaf));
        ws1.insert("focused".to_string(), toml::Value::Integer(999));
        ws_table.insert("1".to_string(), toml::Value::Table(ws1));
        table.insert("workspaces".to_string(), toml::Value::Table(ws_table));

        let Restored {
            workspaces: ws,
            warnings,
            ..
        } = from_toml(&table).expect("dangling focused must be healed, not rejected");
        assert_eq!(
            warnings,
            vec!["missing config_version (assuming 1)".to_string()],
            "a dangling focused reference must not itself add a warning"
        );
        // Dock-trees review fix: the dangling reference heals to the first
        // tile (via `Workspace::from_parts`), not to None — a non-empty
        // restored tree must always have a focused tile, or the move-back
        // verb could hand a tile to a focus-less `split`.
        assert_eq!(ws.active().tree().focused(), Some(TileId(1)));
        assert_eq!(ws.active().tree().tiles(), vec![TileId(1)]);
    }

    // --- config_version (fix round 1, Finding 2) ------------------------

    #[test]
    fn from_toml_rejects_a_mismatched_config_version() {
        let mut table = toml::Table::new();
        table.insert("config_version".to_string(), toml::Value::Integer(99));
        let err =
            from_toml(&table).expect_err("a mismatched version must invalidate the whole session");
        assert_eq!(err.len(), 1);
        assert!(
            err[0].contains("99"),
            "the error must name the version found: {err:?}"
        );
    }

    #[test]
    fn from_toml_accepts_a_matching_config_version_with_no_warning() {
        let mut table = toml::Table::new();
        table.insert(
            "config_version".to_string(),
            toml::Value::Integer(SESSION_CONFIG_VERSION),
        );
        let Restored {
            workspaces: _,
            warnings,
            ..
        } = from_toml(&table).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn from_toml_warns_but_proceeds_on_a_missing_config_version() {
        // Covered end-to-end above
        // (`from_toml_on_an_empty_table_is_a_fresh_workspace_one_with_a_missing_version_warning`,
        // `from_toml_heals_a_dangling_focused_reference`); this test pins
        // the exact warning wording as its own regression.
        let table = toml::Table::new();
        let Restored {
            workspaces: _,
            warnings,
            ..
        } = from_toml(&table).unwrap();
        assert_eq!(
            warnings,
            vec!["missing config_version (assuming 1)".to_string()]
        );
    }

    #[test]
    fn from_toml_rejects_active_out_of_range() {
        let mut table = toml::Table::new();
        table.insert("active".to_string(), toml::Value::Integer(42));
        assert!(from_toml(&table).is_err());
    }

    #[test]
    fn from_toml_rejects_a_workspace_that_is_not_a_table() {
        let mut table = toml::Table::new();
        let mut ws_table = toml::Table::new();
        ws_table.insert("1".to_string(), toml::Value::String("nope".to_string()));
        table.insert("workspaces".to_string(), toml::Value::Table(ws_table));
        assert!(from_toml(&table).is_err());
    }

    #[test]
    fn from_toml_rejects_an_out_of_range_workspace_key() {
        let mut table = toml::Table::new();
        let mut ws_table = toml::Table::new();
        ws_table.insert("42".to_string(), toml::Value::Table(toml::Table::new()));
        table.insert("workspaces".to_string(), toml::Value::Table(ws_table));
        assert!(from_toml(&table).is_err());
    }

    // --- save / load (I/O) ------------------------------------------------

    #[test]
    fn load_of_a_missing_file_is_a_fresh_start_with_no_warnings() {
        let dir = tempfile::tempdir().unwrap();
        let Restored {
            workspaces: ws,
            warnings,
            ..
        } = load(&dir.path().join("session.toml"));
        assert!(warnings.is_empty());
        assert_eq!(ws.active_index(), 1);
        assert!(ws.active().is_empty());
    }

    #[test]
    fn load_of_malformed_toml_is_a_fresh_start_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        std::fs::write(&path, "this is [not valid toml").unwrap();
        let Restored {
            workspaces: ws,
            warnings,
            ..
        } = load(&path);
        assert!(!warnings.is_empty());
        assert!(ws.active().is_empty());
    }

    #[test]
    fn load_of_structurally_invalid_session_is_a_fresh_start_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        std::fs::write(&path, "active = 99\n").unwrap();
        let Restored {
            workspaces: ws,
            warnings,
            ..
        } = load(&path);
        assert!(!warnings.is_empty());
        assert_eq!(ws.active_index(), 1);
    }

    #[test]
    fn load_of_a_mismatched_config_version_is_a_fresh_start_with_a_warning_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        std::fs::write(&path, "config_version = 99\nactive = 3\n").unwrap();
        let Restored {
            workspaces: ws,
            warnings,
            ..
        } = load(&path);
        assert_eq!(
            ws.active_index(),
            1,
            "a version mismatch must be a fresh start"
        );
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("99"),
            "the warning must name the version found: {warnings:?}"
        );
    }

    #[test]
    fn load_of_a_missing_config_version_still_restores_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        std::fs::write(&path, "active = 2\n").unwrap();
        let Restored {
            workspaces: ws,
            warnings,
            ..
        } = load(&path);
        assert_eq!(
            ws.active_index(),
            2,
            "a missing config_version must warn, not discard the rest of the file"
        );
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("config_version"));
    }

    #[test]
    fn save_then_load_round_trips_through_real_files_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");

        let mut ws = Workspaces::new();
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        apply_workspace_action(&mut ws, &act("workspace::split_right"));

        save(&path, &ws, &TileRecords::new(), None).unwrap();
        assert!(path.exists());
        // The atomic-write temp file must not be left behind, whatever its
        // (now pid+counter-suffixed, fix wave Fix 2) exact name was.
        let leftover_tmp_files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "tmp"))
            .collect();
        assert!(
            leftover_tmp_files.is_empty(),
            "no *.tmp files should remain in the session directory, found {leftover_tmp_files:?}"
        );

        let Restored {
            workspaces: restored,
            warnings,
            ..
        } = load(&path);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            restored.active().tree().layout(crate::tiling::Rect::UNIT),
            ws.active().tree().layout(crate::tiling::Rect::UNIT)
        );
    }

    #[test]
    fn save_does_not_write_a_theme_mode_or_extra_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        save(&path, &Workspaces::new(), &TileRecords::new(), None).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("theme_mode"), "{text}");
        assert!(!text.contains("[extra]"), "{text}");
    }

    #[test]
    fn save_creates_the_parent_directory_if_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("session.toml");
        save(&path, &Workspaces::new(), &TileRecords::new(), None).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn a_restored_session_then_splitting_does_not_collide_tile_ids() {
        let mut ws = Workspaces::new();
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        let table = to_toml(&ws, &TileRecords::new(), None);
        let Restored {
            workspaces: mut restored,
            warnings,
            ..
        } = from_toml(&table).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");

        let before_ids: std::collections::HashSet<_> =
            restored.active().tree().tiles().into_iter().collect();

        apply_workspace_action(&mut restored, &act("workspace::split_right"));
        let after_ids: Vec<_> = restored.active().tree().tiles();
        let new_id = after_ids
            .iter()
            .find(|id| !before_ids.contains(id))
            .expect("split must have created a new tile");
        assert!(
            !before_ids.contains(new_id),
            "newly allocated tile id must not collide with a restored id"
        );

        apply_workspace_action(&mut restored, &act("workspace::focus_left"));
        let _ = restored.active().tree().neighbor(Direction::Right);
    }

    // --- docks (dock-regions task, trees per dock-trees task) ------------

    use crate::tiling::{DOCK_DEFAULT_SIZE, DockSide, FocusRegion};

    /// One tile in the tree, a *split* (two-tile) visible left dock
    /// (resized), focus on the dock — dock-trees task: the fixture
    /// exercises the recursive dock encoding, not just a single leaf.
    fn docked_workspaces() -> Workspaces {
        let mut ws = Workspaces::new();
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        apply_workspace_action(&mut ws, &act("workspace::split_down"));
        apply_workspace_action(&mut ws, &act("workspace::resize_right"));
        ws
    }

    #[test]
    fn round_trips_docks_and_region() {
        let ws = docked_workspaces();
        let table = to_toml(&ws, &TileRecords::new(), None);
        let Restored {
            workspaces: restored,
            warnings,
            ..
        } = from_toml(&table).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");

        let before = ws.active();
        let after = restored.active();
        assert_eq!(after.region(), before.region());
        assert_eq!(after.region(), FocusRegion::Dock(DockSide::Left));
        for side in DockSide::ALL {
            let b = before.docks().get(side);
            let a = after.docks().get(side);
            // The whole dock tree round-trips: structure, ratios, and
            // focus memory (Tree derives PartialEq for exactly this).
            assert_eq!(a.tree(), b.tree(), "{side:?}");
            assert_eq!(a.visible(), b.visible(), "{side:?}");
            assert!(
                (a.size() - b.size()).abs() < 1e-4,
                "{side:?}: {} vs {}",
                a.size(),
                b.size()
            );
        }
        assert_eq!(
            before.docks().get(DockSide::Left).tree().tiles().len(),
            2,
            "fixture sanity: the dock really is a split tree"
        );
        assert_eq!(
            after.tree().layout(crate::tiling::Rect::UNIT),
            before.tree().layout(crate::tiling::Rect::UNIT)
        );
    }

    #[test]
    fn a_default_dock_session_writes_no_dock_keys() {
        // Pre-dock-shaped state must keep producing pre-dock-shaped files.
        let mut ws = Workspaces::new();
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        let text = to_string_pretty(&ws, &TileRecords::new(), None).unwrap();
        assert!(!text.contains("docks"), "{text}");
        assert!(!text.contains("region"), "{text}");
    }

    #[test]
    fn restored_docked_tile_ids_do_not_collide_with_new_allocations() {
        let ws = docked_workspaces();
        let table = to_toml(&ws, &TileRecords::new(), None);
        let Restored {
            workspaces: mut restored,
            warnings: _,
            ..
        } = from_toml(&table).unwrap();
        let known: Vec<_> = restored
            .active()
            .tree()
            .tiles()
            .into_iter()
            .chain(restored.active().docks().tiles())
            .collect();
        // Focus is restored into the left dock; step back to Main so the
        // fresh split lands in the main tree (a dock-focused split would
        // work too — same allocator — but Main keeps the assertion simple).
        apply_workspace_action(&mut restored, &act("workspace::focus_right"));
        apply_workspace_action(&mut restored, &act("workspace::split_right"));
        let new_id = restored
            .active()
            .tree()
            .tiles()
            .into_iter()
            .find(|id| !known.contains(id))
            .expect("split created a tile");
        assert!(
            !known.contains(&new_id),
            "next_tile rescan must cover dock tiles"
        );
    }

    #[test]
    fn from_toml_drops_a_dock_tile_also_present_in_the_tree() {
        let mut table = to_toml(&Workspaces::new(), &TileRecords::new(), None);
        let ws1_text = r#"
            focused = 1
            region = "left"
            [node]
            kind = "leaf"
            id = 1
            [docks.left]
            tile = 1
            visible = true
            size = 0.25
        "#;
        let ws1: toml::Table = ws1_text.parse().unwrap();
        if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
            ws_table.insert("1".to_string(), toml::Value::Table(ws1));
        }
        let Restored {
            workspaces: ws,
            warnings,
            ..
        } = from_toml(&table).expect("a duplicate dock claim heals, not fails");
        assert!(
            warnings.iter().any(|w| w.contains("removing")),
            "{warnings:?}"
        );
        assert!(ws.active().docks().get(DockSide::Left).tree().is_empty());
        assert_eq!(ws.active().tree().tiles(), vec![TileId(1)]);
        assert_eq!(
            ws.active().region(),
            FocusRegion::Main,
            "the region pointing at the dropped claim must heal to Main"
        );
    }

    #[test]
    fn from_toml_heals_a_region_pointing_at_a_hidden_or_absent_dock() {
        let mut table = to_toml(&Workspaces::new(), &TileRecords::new(), None);
        let ws1: toml::Table = r#"
            focused = 1
            region = "bottom"
            [node]
            kind = "leaf"
            id = 1
        "#
        .parse()
        .unwrap();
        if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
            ws_table.insert("1".to_string(), toml::Value::Table(ws1));
        }
        let Restored {
            workspaces: ws,
            warnings,
            ..
        } = from_toml(&table).unwrap();
        assert!(!warnings.is_empty(), "{warnings:?}");
        assert_eq!(ws.active().region(), FocusRegion::Main);
    }

    #[test]
    fn from_toml_heals_an_out_of_range_or_nan_dock_size_to_default() {
        for bad in ["size = 0.9", "size = -3.0", "size = nan"] {
            let mut table = to_toml(&Workspaces::new(), &TileRecords::new(), None);
            let ws1: toml::Table = format!(
                r#"
                    [docks.right]
                    visible = true
                    {bad}
                "#
            )
            .parse()
            .unwrap();
            if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
                ws_table.insert("1".to_string(), toml::Value::Table(ws1));
            }
            let Restored {
                workspaces: ws,
                warnings,
                ..
            } = from_toml(&table).expect(bad);
            assert!(
                warnings.iter().any(|w| w.contains("size")),
                "{bad}: {warnings:?}"
            );
            let dock = ws.active().docks().get(DockSide::Right);
            assert!(
                (dock.size() - DOCK_DEFAULT_SIZE).abs() < 1e-4,
                "{bad}: {}",
                dock.size()
            );
            assert!(dock.visible(), "{bad}: healing size must not clear visible");
        }
    }

    #[test]
    fn from_toml_clears_fullscreen_when_the_region_is_a_focusable_dock() {
        // Review should-fix: a hand-edited file claiming tree fullscreen
        // AND focus in a visible occupied dock — contradictory (unreachable
        // live: fullscreen blocks focus from entering docks, and mod+f is
        // a no-op while dock-focused). Heals by clearing fullscreen and
        // keeping the dock focus, with a warning.
        let mut table = to_toml(&Workspaces::new(), &TileRecords::new(), None);
        let ws1: toml::Table = r#"
            focused = 1
            fullscreen = 1
            region = "left"
            [node]
            kind = "leaf"
            id = 1
            [docks.left]
            tile = 2
            visible = true
            size = 0.25
        "#
        .parse()
        .unwrap();
        if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
            ws_table.insert("1".to_string(), toml::Value::Table(ws1));
        }
        let Restored {
            workspaces: ws,
            warnings,
            ..
        } = from_toml(&table).expect("the contradictory combo heals, never fails");
        assert!(
            warnings.iter().any(|w| w.contains("fullscreen")),
            "{warnings:?}"
        );
        assert_eq!(ws.active().tree().fullscreen(), None);
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
        assert_eq!(
            ws.active().docks().get(DockSide::Left).tree().tiles(),
            vec![TileId(2)]
        );
    }

    #[test]
    fn from_toml_heals_an_unknown_region_string_to_main() {
        let mut table = to_toml(&Workspaces::new(), &TileRecords::new(), None);
        let ws1: toml::Table = r#"region = "sideways""#.parse().unwrap();
        if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
            ws_table.insert("1".to_string(), toml::Value::Table(ws1));
        }
        let Restored {
            workspaces: ws,
            warnings,
            ..
        } = from_toml(&table).unwrap();
        assert!(
            warnings.iter().any(|w| w.contains("region")),
            "{warnings:?}"
        );
        assert_eq!(ws.active().region(), FocusRegion::Main);
    }

    #[test]
    fn from_toml_tolerates_hostile_dock_shapes_without_failing() {
        // docks not a table; a side not a table; tile negative/non-integer;
        // visible non-bool — every one heals with a warning, none fails.
        let mut table = to_toml(&Workspaces::new(), &TileRecords::new(), None);
        let ws1: toml::Table = r#"
            [node]
            kind = "leaf"
            id = 1
            [docks.left]
            tile = -5
            visible = "yes"
            [docks.right]
            tile = 2
            visible = true
        "#
        .parse()
        .unwrap();
        if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
            ws_table.insert("1".to_string(), toml::Value::Table(ws1));
        }
        let Restored {
            workspaces: ws,
            warnings,
            ..
        } = from_toml(&table).unwrap();
        assert!(warnings.len() >= 2, "{warnings:?}");
        let left = ws.active().docks().get(DockSide::Left);
        assert!(left.tree().is_empty(), "negative tile id dropped");
        assert!(!left.visible(), "non-bool visible heals to hidden");
        assert_eq!(
            ws.active().docks().get(DockSide::Right).tree().tiles(),
            vec![TileId(2)]
        );
    }

    #[test]
    fn a_pre_dock_session_file_loads_with_default_docks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        std::fs::write(
            &path,
            "config_version = 1\nactive = 1\n\n[workspaces.1]\nfocused = 1\n\n\
             [workspaces.1.node]\nkind = \"leaf\"\nid = 1\n",
        )
        .unwrap();
        let Restored {
            workspaces: ws,
            warnings,
            ..
        } = load(&path);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(ws.active().tree().tiles(), vec![TileId(1)]);
        assert_eq!(ws.active().region(), FocusRegion::Main);
        for side in DockSide::ALL {
            let dock = ws.active().docks().get(side);
            assert!(dock.tree().is_empty());
            assert!(!dock.visible());
        }
    }

    // --- dock trees (dock-trees task) -------------------------------------

    #[test]
    fn a_legacy_single_tile_dock_key_loads_as_a_single_leaf_tree() {
        // The first dock-regions build wrote `tile = N`. It still loads —
        // healed into a single-leaf tree with the tile focused, and
        // *silently* (recorded choice: the prior release's own output is
        // not corruption).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        std::fs::write(
            &path,
            "config_version = 1\nactive = 1\n\n[workspaces.1]\nfocused = 1\nregion = \"left\"\n\n\
             [workspaces.1.node]\nkind = \"leaf\"\nid = 1\n\n\
             [workspaces.1.docks.left]\ntile = 2\nvisible = true\nsize = 0.3\n",
        )
        .unwrap();
        let Restored {
            workspaces: ws,
            warnings,
            ..
        } = load(&path);
        assert!(
            warnings.is_empty(),
            "legacy tile must load silently: {warnings:?}"
        );
        let dock = ws.active().docks().get(DockSide::Left);
        assert_eq!(dock.tree().tiles(), vec![TileId(2)]);
        assert_eq!(
            dock.tree().focused(),
            Some(TileId(2)),
            "the healed single-leaf tree focuses its tile"
        );
        assert!(dock.visible());
        assert!((dock.size() - 0.3).abs() < 1e-4);
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
    }

    #[test]
    fn a_dock_with_both_node_and_legacy_tile_picks_the_node_with_a_warning() {
        let mut table = to_toml(&Workspaces::new(), &TileRecords::new(), None);
        let ws1: toml::Table = r#"
            [docks.left]
            tile = 9
            visible = true
            [docks.left.node]
            kind = "leaf"
            id = 2
        "#
        .parse()
        .unwrap();
        if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
            ws_table.insert("1".to_string(), toml::Value::Table(ws1));
        }
        let Restored {
            workspaces: ws,
            warnings,
            ..
        } = from_toml(&table).unwrap();
        assert!(
            warnings.iter().any(|w| w.contains("legacy tile")),
            "{warnings:?}"
        );
        assert_eq!(
            ws.active().docks().get(DockSide::Left).tree().tiles(),
            vec![TileId(2)],
            "the node tree wins; the legacy tile is ignored"
        );
    }

    #[test]
    fn a_dock_claiming_fullscreen_is_healed_with_a_warning() {
        // Dock trees never have fullscreen — a hand-edited `fullscreen`
        // key inside a dock table is ignored (warned), and the dock's
        // tree loads without it.
        let mut table = to_toml(&Workspaces::new(), &TileRecords::new(), None);
        let ws1: toml::Table = r#"
            [docks.bottom]
            visible = true
            focused = 3
            fullscreen = 3
            [docks.bottom.node]
            kind = "leaf"
            id = 3
        "#
        .parse()
        .unwrap();
        if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
            ws_table.insert("1".to_string(), toml::Value::Table(ws1));
        }
        let Restored {
            workspaces: ws,
            warnings,
            ..
        } = from_toml(&table).unwrap();
        assert!(
            warnings.iter().any(|w| w.contains("fullscreen")),
            "{warnings:?}"
        );
        let dock = ws.active().docks().get(DockSide::Bottom);
        assert_eq!(dock.tree().tiles(), vec![TileId(3)]);
        assert_eq!(dock.tree().fullscreen(), None);
    }

    #[test]
    fn an_invalid_dock_node_drops_only_that_docks_tree() {
        // A structurally invalid dock subtree (single-child split) heals
        // to an empty dock with a warning — the workspace's main tree is
        // never discarded over a dock (docks are an adornment).
        let mut table = to_toml(&Workspaces::new(), &TileRecords::new(), None);
        let ws1: toml::Table = r#"
            focused = 1
            [node]
            kind = "leaf"
            id = 1
            [docks.right]
            visible = true
            [docks.right.node]
            kind = "split"
            orientation = "horizontal"
            ratios = [1.0]
            [[docks.right.node.children]]
            kind = "leaf"
            id = 2
        "#
        .parse()
        .unwrap();
        if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
            ws_table.insert("1".to_string(), toml::Value::Table(ws1));
        }
        let Restored {
            workspaces: ws,
            warnings,
            ..
        } = from_toml(&table).expect("a bad dock tree heals, never fails");
        assert!(warnings.iter().any(|w| w.contains("dock")), "{warnings:?}");
        assert!(ws.active().docks().get(DockSide::Right).tree().is_empty());
        assert_eq!(
            ws.active().tree().tiles(),
            vec![TileId(1)],
            "the main tree survives"
        );
    }

    #[test]
    fn duplicate_ids_across_dock_trees_and_the_main_tree_are_healed() {
        // Tile 1 lives in the main tree AND workspace 1's left dock tree
        // AND its right dock tree; tile 2 only in the right dock. The
        // main tree wins, then first dock claim wins; the right dock
        // keeps its unique tile.
        let mut table = to_toml(&Workspaces::new(), &TileRecords::new(), None);
        let ws1: toml::Table = r#"
            focused = 1
            [node]
            kind = "leaf"
            id = 1
            [docks.left]
            visible = true
            [docks.left.node]
            kind = "leaf"
            id = 1
            [docks.right]
            visible = true
            [docks.right.node]
            kind = "split"
            orientation = "horizontal"
            ratios = [0.5, 0.5]
            [[docks.right.node.children]]
            kind = "leaf"
            id = 1
            [[docks.right.node.children]]
            kind = "leaf"
            id = 2
        "#
        .parse()
        .unwrap();
        if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
            ws_table.insert("1".to_string(), toml::Value::Table(ws1));
        }
        let Restored {
            workspaces: ws,
            warnings,
            ..
        } = from_toml(&table).unwrap();
        assert_eq!(
            warnings.iter().filter(|w| w.contains("removing")).count(),
            2,
            "{warnings:?}"
        );
        assert_eq!(ws.active().tree().tiles(), vec![TileId(1)]);
        assert!(ws.active().docks().get(DockSide::Left).tree().is_empty());
        assert_eq!(
            ws.active().docks().get(DockSide::Right).tree().tiles(),
            vec![TileId(2)]
        );
    }

    #[test]
    fn an_intra_dock_tree_duplicate_id_keeps_one_leaf_with_one_warning() {
        // Review fix: a dock tree carrying the SAME id twice (parseable —
        // node_from_toml has no duplicate check). First claim wins: one
        // copy of the tile survives in place, the other is removed, and
        // exactly one warning is emitted for the one healed duplicate
        // (the pre-fix all-copies prune deleted both leaves and, walking
        // a pre-removal snapshot, warned twice).
        let mut table = to_toml(&Workspaces::new(), &TileRecords::new(), None);
        let ws1: toml::Table = r#"
            [docks.left]
            visible = true
            focused = 5
            [docks.left.node]
            kind = "split"
            orientation = "vertical"
            ratios = [0.5, 0.5]
            [[docks.left.node.children]]
            kind = "leaf"
            id = 5
            [[docks.left.node.children]]
            kind = "leaf"
            id = 5
        "#
        .parse()
        .unwrap();
        if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
            ws_table.insert("1".to_string(), toml::Value::Table(ws1));
        }
        let Restored {
            workspaces: ws,
            warnings,
            ..
        } = from_toml(&table).unwrap();
        assert_eq!(
            warnings.iter().filter(|w| w.contains("removing")).count(),
            1,
            "exactly one warning for the one healed duplicate: {warnings:?}"
        );
        let dock = ws.active().docks().get(DockSide::Left);
        assert_eq!(
            dock.tree().tiles(),
            vec![TileId(5)],
            "one copy of the tile must survive"
        );
        assert_eq!(dock.tree().focused(), Some(TileId(5)));
        assert!(dock.focusable(), "the dock stays visible and occupied");
    }

    #[test]
    fn a_dangling_dock_focused_reference_heals_to_the_first_tile() {
        let mut table = to_toml(&Workspaces::new(), &TileRecords::new(), None);
        let ws1: toml::Table = r#"
            [docks.left]
            visible = true
            focused = 999
            [docks.left.node]
            kind = "leaf"
            id = 4
        "#
        .parse()
        .unwrap();
        if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
            ws_table.insert("1".to_string(), toml::Value::Table(ws1));
        }
        let Restored {
            workspaces: ws,
            warnings: _,
            ..
        } = from_toml(&table).unwrap();
        assert_eq!(
            ws.active().docks().get(DockSide::Left).tree().focused(),
            Some(TileId(4)),
            "Tree::from_parts heals the dangling ref to None; Dock::from_parts \
             then refocuses the first tile so the dock's verbs stay usable"
        );
    }

    // --- Phase 4a: [frame] ------------------------------------------

    fn sample_frame_record() -> FrameRecord {
        FrameRecord {
            scope: Scope {
                dimensions: vec![DimensionSelection {
                    column: "book".into(),
                    values: vec!["BK000".into(), "BK001".into()],
                }],
                text: Some("spx".into()),
                expression: Some(parse_expr("delta01 > 100").unwrap()),
                impossible: false,
            },
            active_slot: Some(3),
            as_of: AsOf::At(
                chrono::DateTime::parse_from_rfc3339("2026-09-05T14:05:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            ),
        }
    }

    #[test]
    fn a_frame_record_round_trips_through_session_toml_with_every_field() {
        let record = sample_frame_record();
        let table = to_toml(&Workspaces::new(), &TileRecords::new(), Some(&record));
        assert!(table.contains_key("frame"), "{table:?}");

        let Restored {
            frame: restored,
            warnings,
            ..
        } = from_toml(&table).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(restored, Some(record));
    }

    #[test]
    fn no_frame_record_writes_no_frame_table() {
        let table = to_toml(&Workspaces::new(), &TileRecords::new(), None);
        assert!(!table.contains_key("frame"), "{table:?}");
        let Restored { frame, .. } = from_toml(&table).unwrap();
        assert_eq!(frame, None);
    }

    #[test]
    fn a_bad_as_of_keeps_the_rest_of_the_frame_record_and_warns() {
        let mut record_table = sample_frame_record().to_toml();
        record_table.insert(
            "as_of".into(),
            toml::Value::String("not-a-timestamp".into()),
        );
        let mut warnings = Vec::new();
        let restored = FrameRecord::from_toml(&record_table, &mut warnings);
        assert!(warnings.iter().any(|w| w.contains("as_of")), "{warnings:?}");
        assert_eq!(restored.as_of, AsOf::Live);
        assert_eq!(restored.active_slot, Some(3));
        assert_eq!(
            restored.scope.dimensions,
            sample_frame_record().scope.dimensions
        );
    }
}
