//! Session layout persistence (Task 3): saves/restores the workspace
//! tiling layout (and the active theme mode) across app restarts.
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
//!
//! [extra]
//! theme_mode = "dark"
//! ```
//!
//! Every workspace present in [`Workspaces::spaces`] is written, empty ones
//! included (mirrors the in-memory behavior: "workspaces materialize lazily
//! on first switch and persist" — spec §3.6), so a session restore leaves
//! the *set* of touched workspaces exactly as the user left it, not just the
//! non-empty ones.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::tiling::{Node, Orientation, TileId, Tree, Workspaces};

/// Schema version written into every session file, and enforced on load
/// exactly like `geode_core::config::load_layer` enforces `config_version`
/// for desk/user config docs: present and matching → fine; present and
/// different → the whole session is invalid (an `Err`, naming the version
/// found — `from_toml`'s caller, `load`, treats that as "fresh start,
/// warn"); missing entirely → a warning, but the rest of the file still
/// gets parsed (mirrors `load_layer` treating a missing version as
/// "assume current" rather than a hard failure).
pub const SESSION_CONFIG_VERSION: i64 = 1;

/// Non-workspace session state (brief: `SessionExtra { theme_mode: Option<String> }`).
/// `theme_mode` is `"light"` or `"dark"`, restored *after* `apply_from_config`
/// runs (main.rs), so a session's saved mode — when present — wins over the
/// config's default mode: a runtime `theme::toggle_mode` survives a restart
/// even if `[theme].mode` in `app.toml` still says the old value.
///
/// Refined precedence rule (fix wave, Fix 3 — see
/// `ShellView::session_theme_mode`, the sole writer of this field): the
/// shell writes `Some(mode)` only when the active mode at save time
/// genuinely *diverges* from the mode the current `[theme]` config resolves
/// to on its own (`ThemeService::config_resolved_mode`) — a real, deliberate
/// toggle this session, not just the active mode happening to mirror
/// config. When they agree, it writes `None`. This means an offline edit to
/// `[theme].mode` in `app.toml` (no toggle this session) takes effect on the
/// next restart instead of being permanently fossilized by a stale session
/// value that only ever mirrored the old config in the first place.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionExtra {
    pub theme_mode: Option<String>,
}

/// Serialize `workspaces` and `extra` into a session TOML table (pure, no
/// I/O — see [`save`] for the file-writing wrapper).
pub fn to_toml(workspaces: &Workspaces, extra: &SessionExtra) -> toml::Table {
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
    for (ix, tree) in workspaces.spaces() {
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
        spaces_table.insert(ix.to_string(), toml::Value::Table(ws_table));
    }
    root.insert("workspaces".to_string(), toml::Value::Table(spaces_table));

    if let Some(mode) = &extra.theme_mode {
        let mut extra_table = toml::Table::new();
        extra_table.insert("theme_mode".to_string(), toml::Value::String(mode.clone()));
        root.insert("extra".to_string(), toml::Value::Table(extra_table));
    }

    root
}

/// Deserialize a session TOML table back into `Workspaces` + `SessionExtra`
/// plus any non-fatal warnings (pure, no I/O). Tolerant of unknown keys
/// (only the fields documented at the top of this file are ever read). Any
/// structural corruption — a mismatched `config_version` (see
/// [`SESSION_CONFIG_VERSION`]), a `Split` with a bad arity/ratio-length
/// mismatch or a non-finite/non-positive ratio (see [`Tree::from_parts`]),
/// an unparseable node, an out-of-range `active` — collects into the `Err`
/// variant rather than partially applying; [`load`] treats that as "fresh
/// start, warn". A dangling `focused`/`fullscreen` reference is healed
/// silently by `Tree::from_parts`, not an error; a missing `config_version`
/// is a warning that still lets the rest of the file parse.
pub fn from_toml(
    table: &toml::Table,
) -> Result<(Workspaces, SessionExtra, Vec<String>), Vec<String>> {
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
    if let Some(workspaces_value) = table.get("workspaces") {
        match workspaces_value.as_table() {
            Some(workspaces_table) => {
                for (key, value) in workspaces_table {
                    match parse_workspace(key, value) {
                        Ok((ix, tree)) => {
                            spaces.insert(ix, tree);
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

    let workspaces = Workspaces::from_parts(spaces, active).map_err(|e| vec![e])?;

    let theme_mode = table
        .get("extra")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("theme_mode"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    Ok((workspaces, SessionExtra { theme_mode }, warnings))
}

fn parse_workspace(key: &str, value: &toml::Value) -> Result<(u8, Tree), String> {
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
    Ok((ix, tree))
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
pub fn to_string_pretty(workspaces: &Workspaces, extra: &SessionExtra) -> Result<String, String> {
    let table = to_toml(workspaces, extra);
    toml::to_string_pretty(&table).map_err(|e| e.to_string())
}

/// Process-global counter (fix wave, Fix 2) giving every [`write_atomic`]
/// call in this process a temp filename distinct from every other
/// *concurrent* call, on top of the pid already distinguishing this process
/// from any other one racing on the same session file. `Ordering::Relaxed`
/// is enough — this only needs distinct values, not a synchronization point
/// with any other memory access.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Atomically write already-serialized session `text` to `path`: a temp
/// file in the same directory, `fsync`, then rename over `path` (rename is
/// atomic on the same filesystem — a crash or concurrent read never
/// observes a partial write). Creates the parent directory if it doesn't
/// exist yet.
///
/// This is real, potentially-blocking file I/O (Task 3 fix round 1: a
/// review finding on the first cut of this module, which ran this inline
/// on the UI thread once per workspace-mutating dispatch — holding e.g.
/// shift+h at OS key-repeat, ~20-30 events/sec, could then stall the render
/// thread on a slow filesystem). Callers driven by UI events must run this
/// on a background executor, never inline — see `ShellView`'s ~500ms
/// watcher-tick flush, which is the only per-dispatch path left after that
/// fix; [`save`] (a synchronous, do-everything wrapper) remains fine for
/// off-the-UI-thread callers: direct test use, and the best-effort
/// `on_app_quit` final flush, which fires at most once, at shutdown.
///
/// Temp filename (fix wave, Fix 2): `.session.toml.{pid}-{counter}.tmp`,
/// unique per call rather than the fixed `.session.toml.tmp` this used
/// before. The fixed name was a real race: `ShellView`'s ~500ms watcher
/// tick and the `on_app_quit` best-effort flush can both call this against
/// the *same* `path` close together (quit right after a workspace
/// mutation), and two concurrent writers sharing one temp filename could
/// interleave — one call's `File::create` truncating the other's
/// in-progress write, or one call's `rename` consuming the other's temp
/// file out from under it (an `ENOENT` on the second `rename`, surfaced as
/// a spurious `[session] warning:` line even though the first writer's data
/// was fine). A unique name per call removes that interleaving entirely:
/// each writer only ever touches its own file until its own `rename`.
///
/// The pid+counter suffix keeps the *residual* case honest rather than
/// pretending it away: two writers can still race the final `rename` step
/// itself (both succeed — `rename` is atomic per-call — but whichever
/// finishes second wins, since both target the same `path`). That ordering
/// is acceptable, not a defect: every candidate `text` here is a valid,
/// self-consistent serialization of *some* real session state, so the
/// "wrong" outcome is at worst a slightly stale-but-valid file (e.g. an
/// old periodic flush's rename lands after the quit-time save's rename,
/// so the file on disk reflects state from ~500ms earlier than the very
/// last action) — never a torn/corrupt file, and never a crash. Losing at
/// most one flush interval of session freshness is exactly the tradeoff
/// this module's whole "best-effort, never load-bearing" session design
/// already accepts elsewhere (see this function's own doc above, and
/// `load`'s "never panic or block startup").
///
/// Still a non-`.toml` name so the reload watcher's `*.toml` glob (Task
/// 1c-1, `reload::scan`) never even sees it mid-write, on top of
/// `session.toml` itself already being excluded by name.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "session path has no parent directory",
        )
    })?;
    std::fs::create_dir_all(dir)?;

    let pid = std::process::id();
    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_path = dir.join(format!(".session.toml.{pid}-{counter}.tmp"));
    {
        let mut file = std::fs::File::create(&tmp_path)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Serialize and atomically write in one synchronous call — for callers
/// that don't need [`to_string_pretty`]/[`write_atomic`] split across a
/// UI/background boundary (direct test use; `ShellView`'s best-effort
/// `on_app_quit` flush, which is a one-shot at shutdown, not a per-keystroke
/// hot path).
pub fn save(path: &Path, workspaces: &Workspaces, extra: &SessionExtra) -> std::io::Result<()> {
    let text = to_string_pretty(workspaces, extra)
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
pub fn load(path: &Path) -> (Workspaces, SessionExtra, Vec<String>) {
    let fresh = |warnings: Vec<String>| (Workspaces::new(), SessionExtra::default(), warnings);

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
        Ok((workspaces, extra, warnings)) => (workspaces, extra, warnings),
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

    // --- to_toml / from_toml round-trip ---------------------------------

    #[test]
    fn round_trips_a_fresh_workspaces() {
        let ws = Workspaces::new();
        let extra = SessionExtra {
            theme_mode: Some("dark".to_string()),
        };
        let table = to_toml(&ws, &extra);
        let (restored, restored_extra, warnings) = from_toml(&table).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(restored.active_index(), ws.active_index());
        assert!(restored.active().is_empty());
        assert_eq!(restored_extra, extra);
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

        let extra = SessionExtra {
            theme_mode: Some("light".to_string()),
        };
        let table = to_toml(&ws, &extra);
        let (restored, restored_extra, warnings) = from_toml(&table).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");

        assert_eq!(restored.active_index(), 1);
        assert_eq!(restored_extra, extra);

        let before: BTreeMap<u8, _> = ws
            .spaces()
            .map(|(ix, tree)| {
                (
                    ix,
                    (tree.layout(crate::tiling::Rect::UNIT), tree.fullscreen()),
                )
            })
            .collect();
        let after: BTreeMap<u8, _> = restored
            .spaces()
            .map(|(ix, tree)| {
                (
                    ix,
                    (tree.layout(crate::tiling::Rect::UNIT), tree.fullscreen()),
                )
            })
            .collect();
        assert_eq!(before, after, "every workspace's layout must round-trip");
    }

    #[test]
    fn round_trip_with_no_theme_mode_omits_the_extra_table() {
        let ws = Workspaces::new();
        let table = to_toml(&ws, &SessionExtra::default());
        assert!(
            !table.contains_key("extra"),
            "omit the extra table entirely when there is nothing to say"
        );
        let (_, extra, warnings) = from_toml(&table).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(extra.theme_mode, None);
    }

    #[test]
    fn from_toml_tolerates_unknown_keys() {
        let mut table = to_toml(&Workspaces::new(), &SessionExtra::default());
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
        let (ws, extra, warnings) = from_toml(&toml::Table::new()).unwrap();
        assert_eq!(ws.active_index(), 1);
        assert!(ws.active().is_empty());
        assert_eq!(extra.theme_mode, None);
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

        let (ws, _, warnings) =
            from_toml(&table).expect("dangling focused must be healed, not rejected");
        assert_eq!(
            warnings,
            vec!["missing config_version (assuming 1)".to_string()],
            "a dangling focused reference must not itself add a warning"
        );
        assert_eq!(ws.active().focused(), None);
        assert_eq!(ws.active().tiles(), vec![TileId(1)]);
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
        let (_, _, warnings) = from_toml(&table).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn from_toml_warns_but_proceeds_on_a_missing_config_version() {
        // Covered end-to-end above
        // (`from_toml_on_an_empty_table_is_a_fresh_workspace_one_with_a_missing_version_warning`,
        // `from_toml_heals_a_dangling_focused_reference`); this test pins
        // the exact warning wording as its own regression.
        let table = toml::Table::new();
        let (_, _, warnings) = from_toml(&table).unwrap();
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
        let (ws, extra, warnings) = load(&dir.path().join("session.toml"));
        assert!(warnings.is_empty());
        assert_eq!(ws.active_index(), 1);
        assert!(ws.active().is_empty());
        assert_eq!(extra.theme_mode, None);
    }

    #[test]
    fn load_of_malformed_toml_is_a_fresh_start_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        std::fs::write(&path, "this is [not valid toml").unwrap();
        let (ws, _, warnings) = load(&path);
        assert!(!warnings.is_empty());
        assert!(ws.active().is_empty());
    }

    #[test]
    fn load_of_structurally_invalid_session_is_a_fresh_start_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        std::fs::write(&path, "active = 99\n").unwrap();
        let (ws, _, warnings) = load(&path);
        assert!(!warnings.is_empty());
        assert_eq!(ws.active_index(), 1);
    }

    #[test]
    fn load_of_a_mismatched_config_version_is_a_fresh_start_with_a_warning_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        std::fs::write(&path, "config_version = 99\nactive = 3\n").unwrap();
        let (ws, _, warnings) = load(&path);
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
        let (ws, _, warnings) = load(&path);
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
        let extra = SessionExtra {
            theme_mode: Some("dark".to_string()),
        };

        save(&path, &ws, &extra).unwrap();
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

        let (restored, restored_extra, warnings) = load(&path);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(restored_extra, extra);
        assert_eq!(
            restored.active().layout(crate::tiling::Rect::UNIT),
            ws.active().layout(crate::tiling::Rect::UNIT)
        );
    }

    #[test]
    fn save_creates_the_parent_directory_if_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("session.toml");
        save(&path, &Workspaces::new(), &SessionExtra::default()).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn a_restored_session_then_splitting_does_not_collide_tile_ids() {
        let mut ws = Workspaces::new();
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        let table = to_toml(&ws, &SessionExtra::default());
        let (mut restored, _, warnings) = from_toml(&table).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");

        let before_ids: std::collections::HashSet<_> =
            restored.active().tiles().into_iter().collect();

        apply_workspace_action(&mut restored, &act("workspace::split_right"));
        let after_ids: Vec<_> = restored.active().tiles();
        let new_id = after_ids
            .iter()
            .find(|id| !before_ids.contains(id))
            .expect("split must have created a new tile");
        assert!(
            !before_ids.contains(new_id),
            "newly allocated tile id must not collide with a restored id"
        );

        apply_workspace_action(&mut restored, &act("workspace::focus_left"));
        let _ = restored.active().neighbor(Direction::Right);
    }
}
