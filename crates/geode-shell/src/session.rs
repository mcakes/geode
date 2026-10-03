//! Session persistence for workspace layout, occupants, frame state, and
//! palette usage. Durable preferences such as theme belong in layered config.
//!
//! The file uses version 1 and nested tables. Workspace keys are indices 1–9;
//! all materialized workspaces are written, including empty ones. Trees encode
//! leaves, splits, and stacks with the same shape in main and dock regions:
//! ```toml
//! config_version = 1
//! active = 1
//!
//! [workspaces.1]
//! focused = 2
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
//! kind = "stack"
//! members = [2, 3]
//! active = 0
//! ```
//!
//! Optional `region` selects a dock (`left`, `right`, or `bottom`); omission
//! means main. Each `workspaces.N.docks.<side>` stores `node`, `focused`,
//! `visible`, and `size`. Default docks are omitted. The legacy `tile = N`
//! shape loads as a single-leaf tree; when both shapes exist, `node` wins with
//! a warning. Dock fullscreen is unsupported and ignored with a warning.
//!
//! `workspaces.N.tiles.<id>` stores a module name, opaque state, and the link
//! groups the tile is in: `follow` and `emit`, each a group letter (`"a"` to
//! `"d"`), written only when set. A value that names no group warns and reads
//! as unset; the tile is kept.
//!
//! `[frame]` stores the shared lane's scope, grouping slot, and as-of;
//! `workspaces.N.frame` stores a pinned workspace's own lane in the same
//! format, and its presence means workspace N is pinned. `[links.<letter>]`
//! stores one link group's scope under `scope`, in the encoding `[frame]`
//! uses for its own (`dimensions`, `text`, `expression`, `named`); a group
//! whose scope is empty writes no table, and an absent table reads as an
//! empty scope. `[palette.usage]`
//! stores usage counts and timestamps; `[pages.<kind>]` stores one opaque
//! table per page kind (whether a page was open is not recorded). Unknown
//! keys are ignored on read and not preserved on save.
//!
//! An invalid main tree or session header rejects the entire session. Docks,
//! tile records, frame fields, link group scopes, palette usage, and page
//! tables recover locally where possible;
//! see [`from_toml`]. [`load`] returns a fresh session on read or parse failure.
//! Neither loading nor healing rewrites the file, but subsequent saves replace
//! it with current state, without retaining a backup.
//!
//! Serialization is I/O-free. Periodic saves serialize on the UI thread and
//! write on the background executor; shutdown saves synchronously. Session
//! writes use atomic replacement, with the ordering and durability limits
//! described by [`write_atomic`].

use std::collections::BTreeMap;
use std::path::Path;

use crate::palette_usage::PaletteUsage;
use crate::tiling::{
    DOCK_MAX_SIZE, DOCK_MIN_SIZE, Dock, DockSide, Docks, FocusRegion, Node, Orientation, TileId,
    Tree, Workspace, WorkspaceIx, Workspaces,
};
use geode_core::link::{Group, Membership};
use geode_core::query::AsOf;
use geode_core::scope::{DimensionSelection, Scope, parse_expr};

/// Version written by the serializer. A missing stamp warns and assumes this
/// version; any present value other than the matching integer rejects the
/// whole session.
pub const SESSION_CONFIG_VERSION: i64 = 1;

/// A tile's module name, opaque serialized state, and link groups. Parsing
/// retains the name without consulting the module roster. The shell passes
/// state only to the factory registered for that name, and applies the link
/// groups only to a tile that factory creates.
///
/// An unavailable module displays a placeholder while its original record is
/// retained in `ShellView::unplaced_records` and included in subsequent saves,
/// link groups included: the placeholder itself is in no group. Closing the
/// tile drops that record; filling it with a module replaces it with the live
/// occupant's record.
#[derive(Debug, Clone, PartialEq)]
pub struct TileRecord {
    pub kind: String,
    pub state: toml::Table,
    /// The groups the tile follows and emits into, written as `follow` and
    /// `emit` only when set.
    pub link: Membership,
}

/// Tile records keyed by raw `TileId`, ordered deterministically for serialization.
pub type TileRecords = BTreeMap<u64, TileRecord>;

/// Page state keyed by page kind, one opaque table each. The shell passes a
/// table only to the page factory registered for that kind; a kind with no
/// factory is carried through the next save unchanged.
pub type PageRecords = BTreeMap<String, toml::Table>;

/// Pinned workspaces' own frame lanes, written as `workspaces.N.frame`. A key
/// present here means that workspace is pinned; an absent key is unpinned.
pub type PinnedRecords = BTreeMap<WorkspaceIx, FrameRecord>;

/// Each link group's scope, in `Group::ALL` order, written as
/// `[links.<letter>]`. An empty scope is a group nothing was written to and
/// has no table.
pub type GroupScopes = [Scope; 4];

/// What [`load`]/[`from_toml`] hand back: the restored layout, each tile's
/// module record, the frame's own restored state if the file had one, page
/// state, and any non-fatal warnings accumulated healing any of it.
#[derive(Debug)]
pub struct Restored {
    pub workspaces: Workspaces,
    pub tiles: TileRecords,
    pub frame: Option<FrameRecord>,
    /// Pinned workspace lanes from `workspaces.N.frame`; empty when none is pinned.
    pub pinned: PinnedRecords,
    /// Link group scopes from `[links.<letter>]`; empty where the file has
    /// no table for a group.
    pub links: GroupScopes,
    /// Palette usage from `[palette.usage]`; absent history starts empty.
    pub palette_usage: PaletteUsage,
    /// Page state from `[pages.<kind>]`; an absent table restores as empty.
    pub pages: PageRecords,
    pub warnings: Vec<String>,
}

/// Restorable frame state: scope, grouping (active slot, or the ad hoc chain),
/// and as-of. The shell applies it to the new frame and clears scope history
/// so restoration does not create an undo step back to the initial empty
/// scope.
///
/// Undo/redo history and recent publishes are transient. Saved scopes come from
/// configuration. These are not part of the session record.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameRecord {
    pub scope: Scope,
    pub active_slot: Option<u8>,
    /// The lane's stored ad hoc chain, whether or not it is the choice.
    pub ad_hoc: Option<Vec<String>>,
    /// Whether the ad hoc chain is the lane's choice. When set, `ad_hoc` is
    /// `Some` and `active_slot` is `None`.
    pub ad_hoc_active: bool,
    pub as_of: AsOf,
}

/// Write a scope's fields into `t`: `dimensions`, `text`, `expression` and
/// `named`, each only when set, omitting empty dimension selections. The
/// one scope encoding of the session file: `[frame]`, a pinned workspace's
/// frame and a link group's `scope` all use it. `Scope::impossible` is not
/// written.
fn scope_to_toml(scope: &Scope, t: &mut toml::Table) {
    let mut dims = toml::Table::new();
    for d in scope.dimensions.iter().filter(|d| !d.values.is_empty()) {
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
    // An absent dimensions table restores as an empty selection.
    if !dims.is_empty() {
        t.insert("dimensions".into(), toml::Value::Table(dims));
    }
    if let Some(text) = &scope.text {
        t.insert("text".into(), toml::Value::String(text.clone()));
    }
    if let Some(e) = &scope.expression {
        t.insert("expression".into(), toml::Value::String(e.to_string()));
    }
    if !scope.named.is_empty() {
        t.insert(
            "named".into(),
            toml::Value::Array(
                scope
                    .named
                    .iter()
                    .map(|n| toml::Value::String(n.clone()))
                    .collect(),
            ),
        );
    }
}

/// Read the scope [`scope_to_toml`] wrote into `t`, without schema or
/// dataset validation. Invalid expression syntax and non-array dimension
/// entries warn, naming `what` (the table being read); other wrong-type
/// fields and non-string dimension values are ignored. Unknown column
/// names remain, and `Scope::impossible` resets to false.
fn scope_from_toml(t: &toml::Table, what: &str, warnings: &mut Vec<String>) -> Scope {
    let mut dimensions = Vec::new();
    if let Some(dims_table) = t.get("dimensions").and_then(|v| v.as_table()) {
        for (column, values) in dims_table {
            let Some(arr) = values.as_array() else {
                warnings.push(format!(
                    "{what}: dimension '{column}' is not an array; ignored"
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
                warnings.push(format!("{what}: expression '{s}': {e}; dropped"));
                None
            }
        });
    // Names are kept without checking they are defined; the frame's
    // `effective_scope` reports a missing one when a tile queries.
    let mut named: Vec<String> = Vec::new();
    match t.get("named") {
        None => {}
        Some(toml::Value::Array(a)) => {
            for s in a.iter().filter_map(|v| v.as_str()) {
                if !named.iter().any(|n| n == s) {
                    named.push(s.to_string());
                }
            }
        }
        Some(_) => warnings.push(format!(
            "{what}: named must be an array of strings; ignored"
        )),
    }
    Scope {
        dimensions,
        text,
        expression,
        impossible: false,
        named,
    }
}

/// Add each link group's scope to a serialized session as
/// `[links.<letter>]`, its `scope` in [`scope_to_toml`]'s encoding. A group
/// whose scope encodes to nothing writes no table, and with no group to
/// write, no `links` table at all. `true` when anything was inserted.
///
/// Separate from [`to_toml`] because the periodic writer compares the
/// session without these tables: a group's scope moves with an emitting
/// tile's cursor, and must not by itself rewrite the file.
pub fn insert_links(root: &mut toml::Table, links: &GroupScopes) -> bool {
    let mut table = toml::Table::new();
    for group in Group::ALL {
        let mut scope = toml::Table::new();
        scope_to_toml(&links[group.index()], &mut scope);
        if scope.is_empty() {
            continue;
        }
        let mut entry = toml::Table::new();
        entry.insert("scope".to_string(), toml::Value::Table(scope));
        table.insert(group.as_str().to_string(), toml::Value::Table(entry));
    }
    if table.is_empty() {
        return false;
    }
    root.insert("links".to_string(), toml::Value::Table(table));
    true
}

/// Read `[links.<letter>]`. An absent table, or a group table with no
/// `scope`, is an empty scope. A key that names no group, a group entry
/// that is not a table and a `scope` that is not a table each warn and are
/// dropped; the other groups still read.
fn links_from_toml(value: Option<&toml::Value>, warnings: &mut Vec<String>) -> GroupScopes {
    let mut links = GroupScopes::default();
    let table = match value {
        None => return links,
        Some(toml::Value::Table(t)) => t,
        Some(_) => {
            warnings.push("links is not a table; ignored".to_string());
            return links;
        }
    };
    for (key, entry) in table {
        let Some(group) = Group::parse(key) else {
            warnings.push(format!("links.{key} names no link group; ignored"));
            continue;
        };
        let Some(entry) = entry.as_table() else {
            warnings.push(format!("links.{key} is not a table; ignored"));
            continue;
        };
        match entry.get("scope") {
            None => {}
            Some(toml::Value::Table(scope)) => {
                links[group.index()] =
                    scope_from_toml(scope, &format!("links.{key}.scope"), warnings);
            }
            Some(_) => warnings.push(format!("links.{key}.scope is not a table; ignored")),
        }
    }
    links
}

impl FrameRecord {
    /// Serialize frame fields, omitting empty dimension selections, an unset
    /// slot, an absent ad hoc chain, and live as-of. The caller decides
    /// whether to include `[frame]`.
    pub fn to_toml(&self) -> toml::Table {
        let mut t = toml::Table::new();
        scope_to_toml(&self.scope, &mut t);
        if let Some(n) = self.active_slot {
            t.insert("slot".into(), toml::Value::Integer(n as i64));
        }
        if let Some(chain) = &self.ad_hoc {
            t.insert(
                "ad_hoc".into(),
                toml::Value::Array(chain.iter().cloned().map(toml::Value::String).collect()),
            );
            if self.ad_hoc_active {
                t.insert("grouping".into(), toml::Value::String("ad_hoc".into()));
            }
        }
        if let AsOf::At(at) = &self.as_of {
            t.insert("as_of".into(), toml::Value::String(at.to_rfc3339()));
        }
        t
    }

    /// Restore usable fields without schema or dataset validation. Invalid
    /// slots, expression syntax, date strings, and non-array dimension entries
    /// warn; other wrong-type fields and non-string dimension values are ignored.
    /// Unknown column names remain, and `Scope::impossible` resets to false.
    /// A malformed `ad_hoc` warns and is ignored; `grouping = "ad_hoc"`
    /// without a usable chain warns and falls back to the slot.
    pub fn from_toml(t: &toml::Table, warnings: &mut Vec<String>) -> FrameRecord {
        let scope = scope_from_toml(t, "frame", warnings);
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
        // A chain naming a column twice is refused whole rather than
        // deduplicated: narrowing a stored chain could restore a grouping the
        // user never chose.
        let ad_hoc = match t.get("ad_hoc") {
            None => None,
            Some(value) => {
                let names: Option<Vec<String>> = value.as_array().and_then(|a| {
                    a.iter()
                        .map(|v| v.as_str().map(str::to_string))
                        .collect::<Option<Vec<String>>>()
                });
                match names {
                    Some(names)
                        if !names.is_empty()
                            && names
                                .iter()
                                .enumerate()
                                .all(|(i, n)| !names[..i].contains(n)) =>
                    {
                        Some(names)
                    }
                    _ => {
                        warnings.push(
                            "frame: ad_hoc is not a non-empty list of distinct column names; ignored"
                                .to_string(),
                        );
                        None
                    }
                }
            }
        };
        let ad_hoc_active = match t.get("grouping") {
            None => false,
            Some(value) => match value.as_str() {
                Some("ad_hoc") if ad_hoc.is_some() => true,
                Some("ad_hoc") => {
                    warnings.push(
                        "frame: grouping is ad_hoc but no ad_hoc chain was restored; ignored"
                            .to_string(),
                    );
                    false
                }
                _ => {
                    warnings.push(format!(
                        "frame: grouping {value} is not \"ad_hoc\"; ignored"
                    ));
                    false
                }
            },
        };
        // The record's invariant: an active ad hoc chain has no slot beside it.
        let active_slot = if ad_hoc_active { None } else { active_slot };
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
            scope,
            active_slot,
            ad_hoc,
            ad_hoc_active,
            as_of,
        }
    }
}

/// Serialize all materialized workspaces without I/O. Write tile records only
/// under the workspace whose main or dock tree contains the ID; omit stale
/// records. Include `[frame]` whenever `frame` is `Some`, even if its table is
/// empty, `workspaces.N.frame` for exactly the workspaces in `pinned`, and
/// palette usage and pages only when nonempty. Link group scopes are not
/// written here: [`insert_links`] adds them.
pub fn to_toml(
    workspaces: &Workspaces,
    tiles: &TileRecords,
    frame: Option<&FrameRecord>,
    pinned: &PinnedRecords,
    palette_usage: &PaletteUsage,
    pages: &PageRecords,
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
        // Omit the main focus region and default docks; absence restores defaults.
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
            // Docks share the main tree's recursive encoding, without fullscreen.
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

        // Only write records for this workspace's main and dock tiles. The
        // records may come from live occupants or preserved unavailable modules;
        // serialization does not resolve module names.
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
            // Absent means "in no group", so a tile that never linked
            // writes the record it always wrote.
            if let Some(g) = record.link.follow {
                t.insert(
                    "follow".to_string(),
                    toml::Value::String(g.as_str().to_string()),
                );
            }
            if let Some(g) = record.link.emit {
                t.insert(
                    "emit".to_string(),
                    toml::Value::String(g.as_str().to_string()),
                );
            }
            tiles_table.insert(id.0.to_string(), toml::Value::Table(t));
        }
        if !tiles_table.is_empty() {
            ws_table.insert("tiles".to_string(), toml::Value::Table(tiles_table));
        }

        // A pinned workspace carries its own lane; presence means pinned, so
        // an unpinned workspace must write none or it would restore pinned.
        if let Some(record) = WorkspaceIx::new(ix).and_then(|w| pinned.get(&w)) {
            ws_table.insert("frame".to_string(), toml::Value::Table(record.to_toml()));
        }

        spaces_table.insert(ix.to_string(), toml::Value::Table(ws_table));
    }
    root.insert("workspaces".to_string(), toml::Value::Table(spaces_table));

    if let Some(record) = frame {
        root.insert("frame".to_string(), toml::Value::Table(record.to_toml()));
    }

    // Omit empty usage history; an absent table restores as empty.
    if !palette_usage.is_empty() {
        let mut palette = toml::Table::new();
        palette.insert(
            "usage".to_string(),
            toml::Value::Table(palette_usage.to_toml()),
        );
        root.insert("palette".to_string(), toml::Value::Table(palette));
    }

    // Omit empty page state; an absent table restores as empty. Each page's
    // table is opaque to the shell, like a tile's `state`.
    if !pages.is_empty() {
        let mut table = toml::Table::new();
        for (kind, state) in pages {
            table.insert(kind.clone(), toml::Value::Table(state.clone()));
        }
        root.insert("pages".to_string(), toml::Value::Table(table));
    }

    root
}

/// Parse session data without I/O, ignoring unknown keys. An unsupported
/// version, invalid workspace index/shape, or unparseable or structurally
/// invalid main tree rejects the entire session. [`load`] converts rejection
/// to a fresh session with warnings.
///
/// Tree validation normalizes positive finite split ratios, heals stack
/// membership and active indices, and drops dangling focus/fullscreen IDs.
/// Workspace restoration supplies focus for nonempty trees. Invalid dock trees
/// are dropped with warnings, and duplicate dock claims are pruned against
/// main trees and earlier docks. A hidden focused dock uses a fallback region;
/// a visible empty dock remains a valid focus target.
///
/// Malformed or locally dangling tile records warn and are dropped. Frame,
/// link group, palette, and page fields recover independently; a non-table
/// `workspaces.N.frame` warns and that workspace restores unpinned, while a
/// table recovers its usable fields like `[frame]`; a missing version warns
/// but still loads. Recovery does not imply schema validation of module, page,
/// or frame state.
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
    let mut pinned = PinnedRecords::new();
    if let Some(workspaces_value) = table.get("workspaces") {
        match workspaces_value.as_table() {
            Some(workspaces_table) => {
                for (key, value) in workspaces_table {
                    match parse_workspace(key, value, &mut warnings, &mut tiles) {
                        Ok((ix, workspace)) => {
                            spaces.insert(ix, workspace);
                            match value.get("frame") {
                                None => {}
                                Some(toml::Value::Table(t)) => {
                                    let record = FrameRecord::from_toml(t, &mut warnings);
                                    // `None` is unreachable: `parse_workspace` bounds `ix` to 1..=9.
                                    if let Some(w) = WorkspaceIx::new(ix) {
                                        pinned.insert(w, record);
                                    }
                                }
                                Some(_) => warnings.push(format!(
                                    "workspaces.{ix}.frame is not a table; workspace {ix} restores unpinned"
                                )),
                            }
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

    // An absent frame is not restored; a wrong-type frame warns and is ignored.
    let frame = match table.get("frame") {
        None => None,
        Some(toml::Value::Table(t)) => Some(FrameRecord::from_toml(t, &mut warnings)),
        Some(_) => {
            warnings.push("frame is not a table; ignored".to_string());
            None
        }
    };

    // Absent group scopes are empty. A malformed entry warns and is
    // dropped without touching the other groups.
    let links = links_from_toml(table.get("links"), &mut warnings);

    // Absent usage is empty. Wrong-type palette or usage tables warn and
    // recover independently of the layout.
    let palette_usage = match table.get("palette") {
        None => PaletteUsage::new(),
        Some(toml::Value::Table(palette)) => match palette.get("usage") {
            None => PaletteUsage::new(),
            Some(toml::Value::Table(usage)) => PaletteUsage::from_toml(usage, &mut warnings),
            Some(_) => {
                warnings.push("palette.usage is not a table; ignored".to_string());
                PaletteUsage::new()
            }
        },
        Some(_) => {
            warnings.push("palette is not a table; ignored".to_string());
            PaletteUsage::new()
        }
    };

    // Absent pages are empty. A wrong-type pages table or page entry warns
    // and is dropped without touching the other kinds.
    let mut pages = PageRecords::new();
    match table.get("pages") {
        None => {}
        Some(toml::Value::Table(t)) => {
            for (kind, value) in t {
                match value {
                    toml::Value::Table(state) => {
                        pages.insert(kind.clone(), state.clone());
                    }
                    _ => warnings.push(format!("pages.{kind} is not a table; ignored")),
                }
            }
        }
        Some(_) => warnings.push("pages is not a table; ignored".to_string()),
    }

    Ok(Restored {
        workspaces,
        tiles,
        frame,
        pinned,
        links,
        palette_usage,
        pages,
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

    // Invalid dock data recovers locally so it cannot discard a valid main tree.
    let docks = parse_docks(ix, ws_table.get("docks"), warnings);
    let region = parse_region(ix, ws_table.get("region"), warnings);

    // Per-workspace healing (duplicate claims against this tree, a
    // region naming a hidden dock) lives in `Workspace::from_parts`; the
    // cross-workspace pass runs later in `Workspaces::from_parts`.
    let (workspace, heal_warnings) = Workspace::from_parts(tree, docks, region);
    warnings.extend(
        heal_warnings
            .into_iter()
            .map(|w| format!("workspace {ix}: {w}")),
    );

    // Validate record membership after local workspace healing. Cross-workspace
    // dock healing runs later in `Workspaces::from_parts`.
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
                    // Keep the module name unchanged. The shell resolves it against the
                    // roster and preserves unavailable modules behind placeholders.
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
                    let link = Membership {
                        follow: parse_group(ix, id, "follow", t, warnings),
                        emit: parse_group(ix, id, "emit", t, warnings),
                    };
                    out_tiles.insert(
                        id,
                        TileRecord {
                            kind: kind.to_string(),
                            state,
                            link,
                        },
                    );
                }
            }
        }
    }

    Ok((ix, workspace))
}

/// Read one link-group key of a tile record. A value that is not a string,
/// or names no group, warns and reads as unset. The tile is kept either way:
/// a bad membership is no reason to lose the tile's module and its state.
fn parse_group(
    ix: u8,
    id: u64,
    key: &str,
    tile: &toml::Table,
    warnings: &mut Vec<String>,
) -> Option<Group> {
    match tile.get(key)? {
        toml::Value::String(s) => {
            let group = Group::parse(s);
            if group.is_none() {
                warnings.push(format!(
                    "workspace {ix}: tile {id} {key} '{s}' is not a link group; ignored"
                ));
            }
            group
        }
        _ => {
            warnings.push(format!(
                "workspace {ix}: tile {id} {key} is not a string; ignored"
            ));
            None
        }
    }
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
        // Accept a recursive node or the legacy single-tile encoding. If both
        // are present, prefer the node and warn; a legacy-only tile loads silently.
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
    // TOML stores signed integers, so IDs must fit in i64 to round-trip.
    // This cast does not check the bound; a wrapped negative leaf or stack ID
    // is rejected on load. Normal allocation starts at small positive IDs.
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
        Node::Stack { children, active } => {
            let mut t = toml::Table::new();
            t.insert("kind".to_string(), toml::Value::String("stack".to_string()));
            t.insert(
                "members".to_string(),
                toml::Value::Array(
                    children
                        .iter()
                        .map(|id| toml::Value::Integer(tile_id_to_i64(*id)))
                        .collect(),
                ),
            );
            t.insert("active".to_string(), toml::Value::Integer(*active as i64));
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
        Some("stack") => {
            let members = table
                .get("members")
                .and_then(|v| v.as_array())
                .ok_or("stack missing 'members' array")?
                .iter()
                .map(|v| {
                    let id = v
                        .as_integer()
                        .ok_or_else(|| "stack member is not an integer".to_string())?;
                    if id < 0 {
                        return Err(format!("stack member id {id} is negative"));
                    }
                    Ok(TileId(id as u64))
                })
                .collect::<Result<Vec<_>, String>>()?;
            // Healing — a short list, a repeated or already-claimed member,
            // an out-of-range `active` — is `Tree::from_parts`'s job
            // (`validate_node`); this reader only refuses what it cannot
            // read at all, exactly as the `leaf` arm does.
            let active = table
                .get("active")
                .and_then(|v| v.as_integer())
                .map(|a| a.max(0) as usize)
                .unwrap_or(0);
            Ok(Node::Stack {
                children: members,
                active,
            })
        }
        other => Err(format!("unknown node kind {other:?}")),
    }
}

/// [`to_toml`] as text: the session without the link groups' scopes, and
/// without filesystem access.
pub fn to_string_pretty(
    workspaces: &Workspaces,
    tiles: &TileRecords,
    frame: Option<&FrameRecord>,
    pinned: &PinnedRecords,
    palette_usage: &PaletteUsage,
    pages: &PageRecords,
) -> Result<String, String> {
    table_to_string(&to_toml(
        workspaces,
        tiles,
        frame,
        pinned,
        palette_usage,
        pages,
    ))
}

/// Replace `path` via `crate::config_write::write_file`: create the parent
/// if needed, write a unique sibling temporary file, sync it, and rename it.
/// Concurrent readers see a complete old or new file. The directory is not
/// fsynced, so system-failure durability is best-effort; errors may leave a
/// temporary file. Writer errors are mapped to `std::io::Error::other`.
///
/// This performs blocking I/O. Periodic saves run it on the background executor;
/// the quit hook calls it synchronously through [`save`]. Session writes bypass
/// the config submission queue and directory transaction lock. Concurrent
/// periodic and shutdown writes have no ordering guarantee: the last rename
/// wins, potentially replacing a newer session with an older snapshot.
///
/// Temporary files end in `.tmp`. The reload scanner also excludes the final
/// `session.toml` filename, so session saves do not trigger config reloads.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    crate::config_write::write_file(path, text).map_err(std::io::Error::other)
}

/// Serialize a session table: [`to_toml`]'s, with or without the tables
/// [`insert_links`] adds.
pub fn table_to_string(table: &toml::Table) -> Result<String, String> {
    toml::to_string_pretty(table).map_err(|e| e.to_string())
}

/// Serialize and write synchronously, link group scopes included. Used by
/// the best-effort quit hook and callers that do not need the UI/background
/// split of the periodic flush.
#[allow(clippy::too_many_arguments)]
pub fn save(
    path: &Path,
    workspaces: &Workspaces,
    tiles: &TileRecords,
    frame: Option<&FrameRecord>,
    pinned: &PinnedRecords,
    links: &GroupScopes,
    palette_usage: &PaletteUsage,
    pages: &PageRecords,
) -> std::io::Result<()> {
    let mut table = to_toml(workspaces, tiles, frame, pinned, palette_usage, pages);
    insert_links(&mut table, links);
    let text = table_to_string(&table)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    write_atomic(path, &text)
}

/// Read the file synchronously. A missing file returns a fresh session without
/// warnings; read, TOML parse, or structural validation failure returns a fresh
/// session with warning strings for the caller to report. Successful recovery
/// retains its local warnings. Loading does not modify the file.
pub fn load(path: &Path) -> Restored {
    let fresh = |warnings: Vec<String>| Restored {
        workspaces: Workspaces::new(),
        tiles: TileRecords::new(),
        frame: None,
        pinned: PinnedRecords::new(),
        links: GroupScopes::default(),
        palette_usage: PaletteUsage::new(),
        pages: PageRecords::new(),
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

    /// An empty usage history, for the tests that serialize a session and
    /// do not care about the palette.
    fn no_usage() -> PaletteUsage {
        PaletteUsage::new()
    }

    /// No page state, for the tests that serialize a session and do not
    /// care about pages.
    fn no_pages() -> PageRecords {
        PageRecords::new()
    }

    fn act(s: &str) -> crate::actions::ActionId {
        crate::actions::ActionId(s.to_string())
    }

    /// Two tiles side by side in workspace 1 — the simplest fixture with
    /// more than one tile to hang a `TileRecord` on. The first
    /// `split_active` on an empty tree only creates the first tile
    /// (nothing to split yet); the second actually splits it in two.
    fn two_tile_workspaces() -> Workspaces {
        let mut ws = Workspaces::new();
        ws.split_active(Orientation::Horizontal);
        ws.split_active(Orientation::Horizontal);
        ws
    }

    // --- Tile records --------------------------------------------------

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
                link: Membership::default(),
            },
        );
        tiles.insert(
            ids[1].0,
            TileRecord {
                kind: "blotter".into(),
                state: toml::Table::new(),
                link: Membership::default(),
            },
        );

        let text = to_string_pretty(
            &ws,
            &tiles,
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        )
        .unwrap();
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
                link: Membership::default(),
            },
        );
        let mut table = to_toml(
            &ws,
            &tiles,
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
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
        let mut table = to_toml(
            &ws,
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
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

    // --- Link membership ------------------------------------------------

    fn linked(follow: Option<Group>, emit: Option<Group>) -> TileRecord {
        TileRecord {
            kind: "blotter".into(),
            state: toml::Table::new(),
            link: Membership { follow, emit },
        }
    }

    fn session_of(ws: &Workspaces, tiles: &TileRecords) -> toml::Table {
        to_toml(
            ws,
            tiles,
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        )
    }

    /// The session table for two unlinked tiles in workspace 1, and the
    /// first tile's id: a record to write a hand-edited key into.
    fn two_unlinked_tiles() -> (toml::Table, u64) {
        let ws = two_tile_workspaces();
        let ids = ws.active().tree().tiles();
        let mut tiles = TileRecords::new();
        tiles.insert(ids[0].0, linked(None, None));
        tiles.insert(ids[1].0, linked(None, None));
        (session_of(&ws, &tiles), ids[0].0)
    }

    fn set_tile_key(table: &mut toml::Table, id: u64, key: &str, value: toml::Value) {
        table["workspaces"]["1"]["tiles"][id.to_string().as_str()]
            .as_table_mut()
            .unwrap()
            .insert(key.into(), value);
    }

    #[test]
    fn a_tiles_link_round_trips_and_absent_keys_mean_none() {
        let ws = two_tile_workspaces();
        let ids = ws.active().tree().tiles();
        let mut tiles = TileRecords::new();
        tiles.insert(ids[0].0, linked(Some(Group::A), Some(Group::B)));
        tiles.insert(ids[1].0, linked(None, None));
        let table = session_of(&ws, &tiles);
        let written = |id: TileId| {
            table["workspaces"]["1"]["tiles"][id.0.to_string().as_str()]
                .as_table()
                .unwrap()
                .clone()
        };
        let first = written(ids[0]);
        assert_eq!(first.get("follow"), Some(&toml::Value::String("a".into())));
        assert_eq!(first.get("emit"), Some(&toml::Value::String("b".into())));
        let second = written(ids[1]);
        assert!(
            !second.contains_key("follow") && !second.contains_key("emit"),
            "a tile in no group writes neither key: {second:?}"
        );

        // Through the file's text as well, which is what a restart reads.
        let text = toml::to_string_pretty(&table).unwrap();
        let restored = from_toml(&text.parse().unwrap()).unwrap();
        assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
        assert_eq!(restored.tiles, tiles);
    }

    #[test]
    fn a_tile_may_follow_or_emit_alone() {
        let ws = two_tile_workspaces();
        let ids = ws.active().tree().tiles();
        let mut tiles = TileRecords::new();
        tiles.insert(ids[0].0, linked(Some(Group::D), None));
        tiles.insert(ids[1].0, linked(None, Some(Group::C)));
        let restored = from_toml(&session_of(&ws, &tiles)).unwrap();
        assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
        assert_eq!(restored.tiles, tiles);
    }

    /// A tile's `state` is a sub-table, and TOML reads every key after a
    /// table header into that table. The file form therefore depends on
    /// the serializer printing `follow` and `emit` above the `state`
    /// header: below it they would be read back as module state.
    #[test]
    fn a_linked_tile_with_state_round_trips_through_the_file_text() {
        let ws = two_tile_workspaces();
        let ids = ws.active().tree().tiles();
        let mut state = toml::Table::new();
        state.insert("view".into(), toml::Value::String("tree".into()));
        let mut nested = toml::Table::new();
        nested.insert("npv".into(), toml::Value::Integer(120));
        state.insert("widths".into(), toml::Value::Table(nested));
        let mut tiles = TileRecords::new();
        tiles.insert(
            ids[0].0,
            TileRecord {
                kind: "blotter".into(),
                state,
                link: Membership {
                    follow: Some(Group::A),
                    emit: Some(Group::B),
                },
            },
        );
        tiles.insert(ids[1].0, linked(None, None));

        let text = to_string_pretty(
            &ws,
            &tiles,
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        )
        .unwrap();
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("{needle} is written: {text}"))
        };
        let state_header = at(&format!("[workspaces.1.tiles.{}.state]", ids[0].0));
        assert!(at("follow = \"a\"") < state_header, "{text}");
        assert!(at("emit = \"b\"") < state_header, "{text}");

        let restored = from_toml(&text.parse().unwrap()).unwrap();
        assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
        assert_eq!(restored.tiles, tiles);
    }

    #[test]
    fn an_unknown_group_is_dropped_with_a_warning_and_the_tile_survives() {
        let (mut table, id) = two_unlinked_tiles();
        set_tile_key(&mut table, id, "follow", toml::Value::String("z".into()));
        set_tile_key(&mut table, id, "emit", toml::Value::String("c".into()));

        let restored = from_toml(&table).unwrap();
        let record = restored.tiles.get(&id).expect("the tile survives");
        assert_eq!(record.kind, "blotter");
        assert_eq!(record.link.follow, None);
        assert_eq!(
            record.link.emit,
            Some(Group::C),
            "the valid key beside it is still read"
        );
        assert_eq!(restored.warnings.len(), 1, "{:?}", restored.warnings);
        let warning = &restored.warnings[0];
        assert!(
            warning.contains(&format!("tile {id}"))
                && warning.contains("follow")
                && warning.contains("'z'"),
            "{warning}"
        );
    }

    #[test]
    fn a_non_string_group_is_dropped_with_a_warning() {
        let (mut table, id) = two_unlinked_tiles();
        set_tile_key(&mut table, id, "emit", toml::Value::Integer(3));

        let restored = from_toml(&table).unwrap();
        let record = restored.tiles.get(&id).expect("the tile survives");
        assert_eq!(record.link, Membership::default());
        assert_eq!(restored.warnings.len(), 1, "{:?}", restored.warnings);
        let warning = &restored.warnings[0];
        assert!(
            warning.contains(&format!("tile {id}")) && warning.contains("emit"),
            "{warning}"
        );
    }

    fn underlying(u: &str) -> Scope {
        Scope::one("underlying_ref", u)
    }

    /// The session table of an empty layout with these group scopes added.
    fn session_with_links(links: &GroupScopes) -> toml::Table {
        let mut table = session_of(&Workspaces::new(), &TileRecords::new());
        insert_links(&mut table, links);
        table
    }

    /// A group's scope is written under its letter in the encoding
    /// `[frame]` uses, and read back equal: every field a scope carries,
    /// through the file's text, which is what a restart reads.
    #[test]
    fn a_group_scope_round_trips_and_an_empty_one_writes_no_table() {
        let mut links = GroupScopes::default();
        links[Group::A.index()] = underlying("SPX.Z");
        links[Group::C.index()] = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK001".into(), "BK002".into()],
            }],
            text: Some("spx".into()),
            expression: Some(parse_expr("npv > 0").unwrap()),
            impossible: false,
            named: vec!["us".into()],
        };
        let table = session_with_links(&links);
        let written = table["links"].as_table().unwrap();
        assert_eq!(
            written.keys().collect::<Vec<_>>(),
            ["a", "c"],
            "a group with an empty scope writes no table: {written:?}"
        );
        // The frame's own encoding: a `[frame]` record of the same scope
        // writes the same fields.
        let as_frame = FrameRecord {
            scope: links[Group::C.index()].clone(),
            active_slot: None,
            ad_hoc: None,
            ad_hoc_active: false,
            as_of: AsOf::Live,
        }
        .to_toml();
        assert_eq!(written["c"]["scope"].as_table(), Some(&as_frame));

        let text = toml::to_string_pretty(&table).unwrap();
        assert!(text.contains("[links.a.scope.dimensions]"), "{text}");
        let restored = from_toml(&text.parse().unwrap()).unwrap();
        assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
        assert_eq!(restored.links, links);

        // No group holds a scope: no `links` table at all, and a file
        // without one reads four empty scopes.
        let bare = session_with_links(&GroupScopes::default());
        assert!(!bare.contains_key("links"), "{bare:?}");
        assert!(!insert_links(
            &mut toml::Table::new(),
            &GroupScopes::default()
        ));
        let restored = from_toml(&bare).unwrap();
        assert_eq!(restored.links, GroupScopes::default());
    }

    /// A malformed group entry is dropped with a warning naming it; the
    /// groups beside it and the rest of the file still read.
    #[test]
    fn a_malformed_group_scope_is_dropped_with_a_warning_and_the_rest_reads() {
        let (mut table, id) = two_unlinked_tiles();
        let links: toml::Table = r#"
            a = 3
            z = { scope = { text = "spx" } }
            [b]
            scope = "everything"
            [c.scope.dimensions]
            underlying_ref = ["NDX"]
            [d.scope]
            expression = "npv >"
            text = "kept"
        "#
        .parse()
        .unwrap();
        table.insert("links".into(), toml::Value::Table(links));

        let restored = from_toml(&table).unwrap();
        assert!(restored.tiles.contains_key(&id), "the layout still reads");
        let mut expected = GroupScopes::default();
        expected[Group::C.index()] = underlying("NDX");
        expected[Group::D.index()] = Scope {
            text: Some("kept".into()),
            ..Scope::default()
        };
        assert_eq!(restored.links, expected);
        let warned = |needle: &str| restored.warnings.iter().any(|w| w.contains(needle));
        assert_eq!(restored.warnings.len(), 4, "{:?}", restored.warnings);
        assert!(warned("links.a is not a table"), "{:?}", restored.warnings);
        assert!(warned("links.z names no link group"));
        assert!(warned("links.b.scope is not a table"));
        assert!(warned("links.d.scope: expression"));

        // A `links` value that is no table at all is ignored whole.
        table.insert("links".into(), toml::Value::Integer(1));
        let restored = from_toml(&table).unwrap();
        assert_eq!(restored.links, GroupScopes::default());
        assert_eq!(restored.warnings, ["links is not a table; ignored"]);
        assert!(restored.tiles.contains_key(&id));
    }

    /// `save` is the quit hook's writer and carries the groups' scopes.
    #[test]
    fn save_writes_the_group_scopes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        let mut links = GroupScopes::default();
        links[Group::B.index()] = underlying("SPX.Z");
        save(
            &path,
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &links,
            &no_usage(),
            &no_pages(),
        )
        .unwrap();
        let restored = load(&path);
        assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
        assert_eq!(restored.links, links);
    }

    #[test]
    fn a_session_without_tiles_still_loads_and_writes_no_tiles_table() {
        // Tile records are optional even when a layout contains tiles.
        let ws = two_tile_workspaces();
        let text = to_string_pretty(
            &ws,
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        )
        .unwrap();
        assert!(!text.contains("tiles"), "{text}");
        let restored = from_toml(&text.parse().unwrap()).unwrap();
        assert!(restored.tiles.is_empty());
    }

    // --- to_toml / from_toml round-trip ---------------------------------

    #[test]
    fn round_trips_a_stack_in_the_main_tree_and_in_a_dock() {
        let mut ws = Workspaces::new();
        let a = ws.split_active(Orientation::Horizontal);
        let b = ws.stack_active().unwrap();
        let _c = ws.stack_active().unwrap();
        ws.active_mut().focus_main_tile(b); // b active, hidden c and a
        let d = ws.split_active(Orientation::Horizontal);
        ws.active_mut().move_to_dock(crate::tiling::DockSide::Left);
        let _e = ws.stack_active().unwrap(); // stack in the dock
        let _ = (a, d);

        let table = to_toml(
            &ws,
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
        let text = toml::to_string(&table).unwrap();
        assert!(text.contains("kind = \"stack\""), "{text}");
        assert!(text.contains("members = ["), "{text}");
        let Restored {
            workspaces: restored,
            warnings,
            ..
        } = from_toml(&table).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(restored.active().tree().tiles(), ws.active().tree().tiles());
        assert_eq!(
            restored.active().tree().visible_tiles(),
            ws.active().tree().visible_tiles()
        );
        assert_eq!(
            restored.active().tree().focused(),
            ws.active().tree().focused()
        );
        let side = crate::tiling::DockSide::Left;
        assert_eq!(
            restored.active().docks().get(side).tree().visible_tiles(),
            ws.active().docks().get(side).tree().visible_tiles()
        );
    }

    #[test]
    fn a_hostile_stack_node_is_healed_not_refused() {
        let text = r#"
config_version = 1
active = 1
[workspaces.1]
focused = 2
[workspaces.1.node]
kind = "split"
orientation = "horizontal"
ratios = [0.5, 0.5]
[[workspaces.1.node.children]]
kind = "leaf"
id = 1
[[workspaces.1.node.children]]
kind = "stack"
members = [1, 2, 3]
active = 12
"#;
        let table: toml::Table = toml::from_str(text).unwrap();
        let Restored { workspaces, .. } = from_toml(&table).unwrap();
        let tree = workspaces.active().tree();
        assert_eq!(tree.tiles(), vec![TileId(1), TileId(2), TileId(3)]);
        assert_eq!(
            tree.stack_position(TileId(2)),
            Some((1, 2)),
            "the leaf's claim on 1 won"
        );
        assert_eq!(
            tree.visible_tiles(),
            vec![TileId(1), TileId(2)],
            "active clamped to 0; focused 2 activated"
        );
    }

    #[test]
    fn a_stack_node_with_a_bad_member_list_is_an_error_like_a_bad_leaf() {
        let text = r#"
config_version = 1
active = 1
[workspaces.1]
[workspaces.1.node]
kind = "stack"
members = [1, -4]
"#;
        let table: toml::Table = toml::from_str(text).unwrap();
        let err = from_toml(&table).unwrap_err();
        assert!(err.iter().any(|e| e.contains("negative")), "{err:?}");
    }

    #[test]
    fn round_trips_a_fresh_workspaces() {
        let ws = Workspaces::new();
        let table = to_toml(
            &ws,
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
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
        ws.split_active(Orientation::Horizontal);
        ws.split_active(Orientation::Horizontal);
        ws.split_active(Orientation::Vertical);
        ws.switch(3);
        ws.split_active(Orientation::Horizontal);
        apply_workspace_action(&mut ws, &act("workspace::fullscreen_tile"));
        ws.switch(1);

        let table = to_toml(
            &ws,
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
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
        // Unknown `[extra]` fields are ignored without discarding the layout.
        let mut table = to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
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
        let mut table = to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
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
        // A dangling focus heals to the first tile via `Workspace::from_parts`.
        // A nonempty restored main tree must have focus for tile movement.
        assert_eq!(ws.active().tree().focused(), Some(TileId(1)));
        assert_eq!(ws.active().tree().tiles(), vec![TileId(1)]);
    }

    // --- Config version ------------------------------------------------

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
        ws.split_active(Orientation::Horizontal);
        ws.split_active(Orientation::Horizontal);

        save(
            &path,
            &ws,
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &GroupScopes::default(),
            &no_usage(),
            &no_pages(),
        )
        .unwrap();
        assert!(path.exists());
        // Successful replacement consumes its temporary file.
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
        save(
            &path,
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &GroupScopes::default(),
            &no_usage(),
            &no_pages(),
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("theme_mode"), "{text}");
        assert!(!text.contains("[extra]"), "{text}");
    }

    #[test]
    fn save_creates_the_parent_directory_if_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("session.toml");
        save(
            &path,
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &GroupScopes::default(),
            &no_usage(),
            &no_pages(),
        )
        .unwrap();
        assert!(path.exists());
    }

    #[test]
    fn a_restored_session_then_splitting_does_not_collide_tile_ids() {
        let mut ws = Workspaces::new();
        ws.split_active(Orientation::Horizontal);
        ws.split_active(Orientation::Horizontal);
        ws.split_active(Orientation::Horizontal);
        let table = to_toml(
            &ws,
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
        let Restored {
            workspaces: mut restored,
            warnings,
            ..
        } = from_toml(&table).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");

        let before_ids: std::collections::HashSet<_> =
            restored.active().tree().tiles().into_iter().collect();

        restored.split_active(Orientation::Horizontal);
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

    // --- Docks ---------------------------------------------------------

    use crate::tiling::{DOCK_DEFAULT_SIZE, DockSide, FocusRegion};

    /// A main leaf and a resized, visible split dock with dock focus exercise
    /// recursive dock serialization.
    fn docked_workspaces() -> Workspaces {
        let mut ws = Workspaces::new();
        ws.split_active(Orientation::Horizontal);
        ws.split_active(Orientation::Horizontal);
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        ws.split_active(Orientation::Vertical);
        apply_workspace_action(&mut ws, &act("workspace::resize_right"));
        ws
    }

    #[test]
    fn round_trips_docks_and_region() {
        let ws = docked_workspaces();
        let table = to_toml(
            &ws,
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
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

    /// A visible empty dock is a valid focus target and must restore without
    /// a warning or a change of focus region.
    #[test]
    fn an_empty_but_visible_focused_dock_round_trips_without_a_warning() {
        let mut ws = Workspaces::new();
        ws.split_active(Orientation::Horizontal);
        apply_workspace_action(&mut ws, &act("dock::toggle_left"));
        assert_eq!(
            ws.active().region(),
            FocusRegion::Dock(DockSide::Left),
            "fixture sanity: toggling shows and focuses the empty left dock"
        );
        assert!(
            ws.active().docks().get(DockSide::Left).tree().is_empty(),
            "fixture sanity: the dock really is empty"
        );

        let table = to_toml(
            &ws,
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
        let Restored {
            workspaces: restored,
            warnings,
            ..
        } = from_toml(&table).unwrap();
        assert!(
            warnings.is_empty(),
            "an empty visible dock is legal; nothing to heal: {warnings:?}"
        );
        assert_eq!(
            restored.active().region(),
            FocusRegion::Dock(DockSide::Left),
            "the region must survive the restore"
        );
        assert!(
            restored.active().docks().get(DockSide::Left).visible(),
            "and the dock is still visible"
        );
    }

    #[test]
    fn a_default_dock_session_writes_no_dock_keys() {
        // Omit dock fields when all docks and the focus region are at defaults.
        let mut ws = Workspaces::new();
        ws.split_active(Orientation::Horizontal);
        let text = to_string_pretty(
            &ws,
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        )
        .unwrap();
        assert!(!text.contains("docks"), "{text}");
        assert!(!text.contains("region"), "{text}");
    }

    #[test]
    fn restored_docked_tile_ids_do_not_collide_with_new_allocations() {
        let ws = docked_workspaces();
        let table = to_toml(
            &ws,
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
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
        restored.split_active(Orientation::Horizontal);
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
        let mut table = to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
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
            FocusRegion::Dock(DockSide::Left),
            "the dock is emptied but still visible, so it keeps the region"
        );
    }

    #[test]
    fn from_toml_heals_a_region_pointing_at_a_hidden_or_absent_dock() {
        let mut table = to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
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
            let mut table = to_toml(
                &Workspaces::new(),
                &TileRecords::new(),
                None,
                &PinnedRecords::new(),
                &no_usage(),
                &no_pages(),
            );
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
        // Main-tree fullscreen and dock focus cannot coexist. Restore dock
        // focus and clear fullscreen with a warning.
        let mut table = to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
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
        let mut table = to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
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
        let mut table = to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
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

    // --- Dock trees ----------------------------------------------------

    #[test]
    fn a_legacy_single_tile_dock_key_loads_as_a_single_leaf_tree() {
        // A legacy `tile = N` loads as a focused single-leaf tree without warnings.
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
        let mut table = to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
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
        let mut table = to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
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
        let mut table = to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
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
        let mut table = to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
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
        // A repeated dock ID keeps its first occurrence, removes the other,
        // and emits one warning for the duplicate.
        let mut table = to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
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
        let mut table = to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
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

    // --- Frame ---------------------------------------------------------

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
                named: Vec::new(),
            },
            active_slot: Some(3),
            ad_hoc: None,
            ad_hoc_active: false,
            as_of: AsOf::At(
                chrono::DateTime::parse_from_rfc3339("2026-09-05T14:05:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            ),
        }
    }

    #[test]
    fn an_active_ad_hoc_chain_round_trips_and_writes_no_slot() {
        let mut record = sample_frame_record();
        record.active_slot = None;
        record.ad_hoc = Some(vec!["underlying_ref".into(), "expiry".into()]);
        record.ad_hoc_active = true;
        let table = record.to_toml();
        assert!(!table.contains_key("slot"), "{table:?}");
        assert_eq!(table["grouping"].as_str(), Some("ad_hoc"));
        let mut warnings = Vec::new();
        let restored = FrameRecord::from_toml(&table, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(restored, record);
    }

    #[test]
    fn a_stored_but_inactive_ad_hoc_chain_round_trips_beside_the_slot() {
        let mut record = sample_frame_record();
        record.active_slot = Some(3);
        record.ad_hoc = Some(vec!["book".into()]);
        record.ad_hoc_active = false;
        let table = record.to_toml();
        assert_eq!(table["slot"].as_integer(), Some(3));
        assert!(!table.contains_key("grouping"), "{table:?}");
        let mut warnings = Vec::new();
        let restored = FrameRecord::from_toml(&table, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(restored, record);
    }

    #[test]
    fn a_record_without_the_ad_hoc_keys_reads_as_it_always_did() {
        let table: toml::Table = toml::from_str("slot = 2\ntext = \"spx\"").unwrap();
        let mut warnings = Vec::new();
        let restored = FrameRecord::from_toml(&table, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(restored.active_slot, Some(2));
        assert_eq!(restored.ad_hoc, None);
        assert!(!restored.ad_hoc_active);
    }

    #[test]
    fn ad_hoc_active_without_a_chain_warns_and_falls_back_to_the_slot() {
        let table: toml::Table = toml::from_str("slot = 2\ngrouping = \"ad_hoc\"").unwrap();
        let mut warnings = Vec::new();
        let restored = FrameRecord::from_toml(&table, &mut warnings);
        assert!(!restored.ad_hoc_active);
        assert_eq!(restored.active_slot, Some(2), "the slot is the fallback");
        assert!(
            warnings.iter().any(|w| w.contains("grouping")),
            "{warnings:?}"
        );
    }

    #[test]
    fn a_malformed_ad_hoc_chain_warns_and_is_ignored() {
        for text in [
            "ad_hoc = \"book\"",
            "ad_hoc = []",
            "ad_hoc = [\"book\", 3]",
            "ad_hoc = [\"book\", \"book\"]",
        ] {
            let table: toml::Table = toml::from_str(text).unwrap();
            let mut warnings = Vec::new();
            let restored = FrameRecord::from_toml(&table, &mut warnings);
            assert_eq!(restored.ad_hoc, None, "{text}");
            assert!(
                warnings.iter().any(|w| w.contains("ad_hoc")),
                "{text}: {warnings:?}"
            );
        }
    }

    #[test]
    fn an_unknown_grouping_value_warns_and_is_ignored() {
        let table: toml::Table =
            toml::from_str("ad_hoc = [\"book\"]\ngrouping = \"slot\"").unwrap();
        let mut warnings = Vec::new();
        let restored = FrameRecord::from_toml(&table, &mut warnings);
        assert_eq!(restored.ad_hoc, Some(vec!["book".to_string()]));
        assert!(!restored.ad_hoc_active);
        assert!(
            warnings.iter().any(|w| w.contains("grouping")),
            "{warnings:?}"
        );
    }

    #[test]
    fn a_pinned_lane_round_trips_under_its_workspace() {
        let mut spaces = Workspaces::new();
        spaces.switch(2);
        let mut pinned = PinnedRecords::new();
        pinned.insert(WorkspaceIx::new(2).unwrap(), sample_frame_record());
        let table = to_toml(
            &spaces,
            &TileRecords::new(),
            None,
            &pinned,
            &no_usage(),
            &no_pages(),
        );
        let ws2 = table["workspaces"]["2"].as_table().unwrap();
        assert!(ws2.contains_key("frame"));
        assert!(
            !table["workspaces"]["1"]
                .as_table()
                .unwrap()
                .contains_key("frame"),
            "an unpinned workspace writes no frame"
        );
        let restored = from_toml(&table).unwrap();
        assert_eq!(restored.pinned, pinned);
    }

    #[test]
    fn a_non_table_workspace_frame_restores_unpinned_with_a_warning() {
        let mut table = to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
        table["workspaces"]["1"]
            .as_table_mut()
            .unwrap()
            .insert("frame".into(), toml::Value::Integer(3));
        let restored = from_toml(&table).unwrap();
        assert!(restored.pinned.is_empty());
        assert!(
            restored
                .warnings
                .iter()
                .any(|w| w.contains("workspaces.1.frame"))
        );
    }

    #[test]
    fn a_partial_pinned_record_keeps_its_usable_fields() {
        let mut table = to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
        let frame: toml::Table = r#"
            slot = 2
            as_of = "not a date"
            named = ["undefined_name"]
        "#
        .parse()
        .unwrap();
        table["workspaces"]["1"]
            .as_table_mut()
            .unwrap()
            .insert("frame".into(), toml::Value::Table(frame));
        let restored = from_toml(&table).unwrap();
        let record = &restored.pinned[&WorkspaceIx::FIRST];
        assert_eq!(record.active_slot, Some(2));
        assert_eq!(
            record.scope.named,
            vec!["undefined_name".to_string()],
            "an undefined name is kept; the lane refuses per query"
        );
        assert_eq!(record.as_of, AsOf::Live);
        assert!(
            restored.warnings.iter().any(|w| w.contains("as_of")),
            "the bad as-of warns"
        );
    }

    #[test]
    fn a_frame_record_round_trips_through_session_toml_with_every_field() {
        let record = sample_frame_record();
        let table = to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            Some(&record),
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
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
    fn a_frame_record_with_no_dimension_selections_writes_no_dimensions_key() {
        // Empty dimension selections are omitted; absence restores the same scope.
        let record = FrameRecord {
            scope: Scope::default(),
            active_slot: None,
            ad_hoc: None,
            ad_hoc_active: false,
            as_of: AsOf::Live,
        };
        let table = record.to_toml();
        assert!(!table.contains_key("dimensions"), "{table:?}");
        let mut warnings = Vec::new();
        let restored = FrameRecord::from_toml(&table, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(restored, record);
    }

    #[test]
    fn frame_named_expressions_round_trip_in_order_and_a_missing_name_is_kept() {
        // Restore does not validate names: "liq" need not be defined anywhere.
        let mut record = sample_frame_record();
        record.scope.named = vec!["liq".into(), "hedges".into()];
        let table = record.to_toml();
        let mut warnings = Vec::new();
        let restored = FrameRecord::from_toml(&table, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(restored, record);
        assert!(
            !sample_frame_record().to_toml().contains_key("named"),
            "an empty list is omitted"
        );
    }

    #[test]
    fn frame_named_is_deduplicated_and_a_non_array_warns_and_is_ignored() {
        let table: toml::Table = toml::from_str("named = [\"liq\", \"liq\", \"b\"]").unwrap();
        let mut warnings = Vec::new();
        let restored = FrameRecord::from_toml(&table, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(restored.scope.named, ["liq", "b"]);

        let table: toml::Table = toml::from_str("named = \"liq\"\ntext = \"spx\"").unwrap();
        let mut warnings = Vec::new();
        let restored = FrameRecord::from_toml(&table, &mut warnings);
        assert!(restored.scope.named.is_empty());
        assert_eq!(restored.scope.text.as_deref(), Some("spx"), "rest kept");
        assert!(warnings.iter().any(|w| w.contains("named")), "{warnings:?}");
    }

    #[test]
    fn no_frame_record_writes_no_frame_table() {
        let table = to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
        assert!(!table.contains_key("frame"), "{table:?}");
        let Restored { frame, .. } = from_toml(&table).unwrap();
        assert_eq!(frame, None);
    }

    // -- palette usage ----------------------------------------------------

    #[test]
    fn palette_usage_round_trips_through_session_toml() {
        let mut usage = PaletteUsage::new();
        usage.record("action:workspace::close_tile", 1_800_000_000);
        usage.record("theme:Gruvbox Dark", 1_800_000_100);
        let text = to_string_pretty(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &usage,
            &no_pages(),
        )
        .unwrap();
        assert!(text.contains("[palette.usage"), "{text}");
        let Restored {
            palette_usage: restored,
            warnings,
            ..
        } = from_toml(&text.parse().unwrap()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(restored, usage);
    }

    #[test]
    fn empty_palette_usage_writes_no_palette_table() {
        let table = to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
        assert!(!table.contains_key("palette"), "{table:?}");
    }

    /// An absent palette table restores empty usage without warnings.
    #[test]
    fn a_session_file_without_a_palette_table_restores_empty_usage_silently() {
        let table = to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
        assert!(!table.contains_key("palette"), "{table:?}");
        let Restored {
            palette_usage,
            warnings,
            ..
        } = from_toml(&table).unwrap();
        assert!(palette_usage.is_empty());
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn a_malformed_palette_table_warns_and_restores_empty_usage() {
        let mut table = to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
        table.insert("palette".to_string(), toml::Value::Integer(3));
        let Restored {
            palette_usage,
            warnings,
            ..
        } = from_toml(&table).unwrap();
        assert!(palette_usage.is_empty());
        assert_eq!(warnings.len(), 1, "{warnings:?}");

        let mut table = to_toml(
            &Workspaces::new(),
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &no_usage(),
            &no_pages(),
        );
        let mut palette = toml::Table::new();
        palette.insert("usage".to_string(), toml::Value::Boolean(true));
        table.insert("palette".to_string(), toml::Value::Table(palette));
        let Restored {
            palette_usage,
            warnings,
            ..
        } = from_toml(&table).unwrap();
        assert!(palette_usage.is_empty());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    // -- pages ------------------------------------------------------------

    #[test]
    fn pages_round_trip_and_an_unknown_kind_is_kept() {
        let workspaces = Workspaces::new();
        let mut pages = PageRecords::new();
        let mut diag = toml::Table::new();
        diag.insert("section".into(), toml::Value::String("log".into()));
        pages.insert("diagnostics".into(), diag);
        let text = to_string_pretty(
            &workspaces,
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &PaletteUsage::new(),
            &pages,
        )
        .unwrap();
        assert!(text.contains("[pages.diagnostics]"), "{text}");
        let Restored {
            pages: restored,
            warnings,
            ..
        } = from_toml(&text.parse::<toml::Table>().unwrap()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(restored, pages);
        // Empty pages are omitted.
        let text = to_string_pretty(
            &workspaces,
            &TileRecords::new(),
            None,
            &PinnedRecords::new(),
            &PaletteUsage::new(),
            &PageRecords::new(),
        )
        .unwrap();
        assert!(!text.contains("[pages"), "{text}");
    }

    #[test]
    fn a_non_table_page_record_warns_and_is_dropped() {
        let text = "config_version = 1\nactive = 1\n[pages]\ndiagnostics = 3\n";
        let Restored {
            pages: restored,
            warnings,
            ..
        } = from_toml(&text.parse::<toml::Table>().unwrap()).unwrap();
        assert!(restored.is_empty());
        assert!(
            warnings.iter().any(|w| w.contains("pages.diagnostics")),
            "{warnings:?}"
        );
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
