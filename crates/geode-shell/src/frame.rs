//! The shared frame (foundation §4, Phase 3 §4): global scope, the active
//! grouping slot, as-of, and the data and config generations, as one
//! value every tile observes. Pure: `ShellView` holds it in a gpui
//! entity and notifies; a module reads it through that entity.
//!
//! Every mutation bumps exactly the counters it affects, so a tile can
//! compare the fields it follows against the ones it last acted on with
//! one integer compare each — a pinned tile ignores `grouping`, an
//! unscoped tile ignores `scope`, every tile follows `as_of`, `data` and
//! `config` (§4.1).

use crate::perf::RequeryStats;
use geode_core::groupings::GroupingSlots;
use geode_core::query::AsOf;
use geode_core::scope::Scope;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, value};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameVersions {
    pub scope: u64,
    pub grouping: u64,
    pub as_of: u64,
    pub data: u64,
    pub config: u64,
}

/// What the title bar shows (§4.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameReadout {
    pub slot: Option<(u8, String)>,
    pub scope: String,
    pub as_of: Option<String>,
}

#[derive(Debug)]
pub struct Frame {
    scope: Scope,
    previous_scope: Option<Scope>,
    slots: GroupingSlots,
    active_slot: Option<u8>,
    as_of: AsOf,
    versions: FrameVersions,
    /// Requery timing the blotter records (Phase 3 §6.8). Here because
    /// the frame is the one shell-side handle every module holds.
    pub requery: RequeryStats,
    user_dir: Option<PathBuf>,
    /// A slot saved by `save_slot`, waiting to be written to the user
    /// layer's `groupings.toml`. The frame is pure (no file access), so
    /// `ShellView` drains this via [`take_pending_persist`](Self::take_pending_persist)
    /// — observed off an entity-change notification — and does the actual
    /// background write with [`persist_slot_to_user_config`] (§4.2).
    pending_persist: Option<(u8, Vec<String>)>,
}

impl Frame {
    pub fn new(slots: GroupingSlots, user_dir: Option<PathBuf>) -> Frame {
        Frame {
            scope: Scope::default(),
            previous_scope: None,
            slots,
            active_slot: None,
            as_of: AsOf::Live,
            versions: FrameVersions::default(),
            requery: RequeryStats::new(),
            user_dir,
            pending_persist: None,
        }
    }

    pub fn versions(&self) -> FrameVersions {
        self.versions
    }

    pub fn user_dir(&self) -> Option<&Path> {
        self.user_dir.as_deref()
    }

    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    /// Replace the global scope, remembering the previous one for
    /// `undo_scope`. `false` when nothing changed.
    pub fn set_scope(&mut self, scope: Scope) -> bool {
        if self.scope == scope {
            return false;
        }
        self.previous_scope = Some(std::mem::replace(&mut self.scope, scope));
        self.versions.scope += 1;
        true
    }

    pub fn clear_scope(&mut self) -> bool {
        self.set_scope(Scope::default())
    }

    /// Restore the previous scope, one level (§4.3). This consumes the
    /// remembered scope rather than swapping it back in, so a second
    /// `undo_scope` in a row does nothing — only a further `set_scope` or
    /// `clear_scope` refills `previous_scope`.
    pub fn undo_scope(&mut self) -> bool {
        let Some(previous) = self.previous_scope.take() else {
            return false;
        };
        self.scope = previous;
        self.versions.scope += 1;
        true
    }

    pub fn slots(&self) -> &GroupingSlots {
        &self.slots
    }

    pub fn active_slot(&self) -> Option<u8> {
        self.active_slot
    }

    pub fn active_grouping(&self) -> Option<&[String]> {
        self.slots.get(self.active_slot?)
    }

    /// `Some(n)` activates a filled slot; `None` returns following tiles
    /// to their views' own grouping. `false` when nothing changed or the
    /// slot is empty.
    pub fn set_active_slot(&mut self, slot: Option<u8>) -> bool {
        if let Some(n) = slot
            && self.slots.get(n).is_none()
        {
            return false;
        }
        if self.active_slot == slot {
            return false;
        }
        self.active_slot = slot;
        self.versions.grouping += 1;
        true
    }

    /// A reloaded `groupings.toml` (§4.5). Bumps `config`, and `grouping`
    /// too because the active slot's contents may have changed; an active
    /// slot that no longer exists is cleared.
    pub fn replace_slots(&mut self, slots: GroupingSlots) -> bool {
        if self.slots == slots {
            return false;
        }
        self.slots = slots;
        if self
            .active_slot
            .is_some_and(|n| self.slots.get(n).is_none())
        {
            self.active_slot = None;
        }
        self.versions.config += 1;
        self.versions.grouping += 1;
        true
    }

    /// `:group save N`: set the slot in memory. The caller persists with
    /// [`persist_slot_to_user_config`] off the UI thread — see
    /// `take_pending_persist`.
    pub fn save_slot(&mut self, slot: u8, grouping: Vec<String>) -> Result<(), String> {
        let persisted = grouping.clone();
        if !self.slots.set(slot, grouping) {
            return Err(format!(
                "slot must be 1–9 and the grouping non-empty (got {slot})"
            ));
        }
        if self.active_slot == Some(slot) {
            self.versions.grouping += 1;
        }
        self.pending_persist = Some((slot, persisted));
        Ok(())
    }

    /// Take the slot a `save_slot` call is waiting to have written to the
    /// user layer's `groupings.toml`, if any (§4.2). `ShellView` calls this
    /// from its frame-change observer and does the actual write on the
    /// background executor.
    pub fn take_pending_persist(&mut self) -> Option<(u8, Vec<String>)> {
        self.pending_persist.take()
    }

    pub fn as_of(&self) -> &AsOf {
        &self.as_of
    }

    pub fn set_as_of(&mut self, as_of: AsOf) -> bool {
        if self.as_of == as_of {
            return false;
        }
        self.as_of = as_of;
        self.versions.as_of += 1;
        true
    }

    pub fn note_published(&mut self) {
        self.versions.data += 1;
    }

    pub fn note_config_reloaded(&mut self) {
        self.versions.config += 1;
    }

    /// Global AND tile (foundation §4.2). Phase 3 passes an empty tile
    /// layer; `:filter` will fill it.
    pub fn effective_scope(&self, tile: &Scope) -> Scope {
        self.scope.and_then(tile)
    }

    pub fn readout(&self) -> FrameReadout {
        let slot = self
            .active_slot
            .and_then(|n| self.slots.label(n).map(|l| (n, l)));
        let mut parts: Vec<String> = Vec::new();
        for d in &self.scope.dimensions {
            if !d.values.is_empty() {
                parts.push(format!("{} ∈ {{{}}}", d.column, d.values.len()));
            }
        }
        if let Some(t) = &self.scope.text {
            parts.push(format!("text \"{t}\""));
        }
        if self.scope.expression.is_some() {
            parts.push("expr".into());
        }
        if self.scope.impossible {
            parts.push("∅".into());
        }
        let as_of = match &self.as_of {
            AsOf::Live => None,
            AsOf::At(t) => Some(t.format("%Y-%m-%d %H:%M").to_string()),
        };
        FrameReadout {
            slot,
            scope: parts.join(" · "),
            as_of,
        }
    }
}

/// Write one slot into the user layer's `groupings.toml` as a bare
/// numeric key, keeping every other key (Phase 3 §4.2). The same
/// `toml_edit` read-modify-write and atomic rename `theme::
/// persist_to_user_config` uses.
pub fn persist_slot_to_user_config(
    user_dir: &Path,
    slot: u8,
    grouping: &[String],
) -> Result<(), String> {
    if !(1..=9).contains(&slot) || grouping.is_empty() {
        return Err(format!("slot {slot} out of range or empty grouping"));
    }
    let path = user_dir.join("groupings.toml");
    let existed = path.exists();
    let mut doc = if existed {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        text.parse::<DocumentMut>().map_err(|e| {
            format!(
                "failed to parse {}: {e} (file left untouched)",
                path.display()
            )
        })?
    } else {
        DocumentMut::new()
    };
    if !existed {
        doc["config_version"] = value(1_i64);
    }
    let mut array = toml_edit::Array::new();
    for g in grouping {
        array.push(g.as_str());
    }
    doc[slot.to_string().as_str()] = value(array);
    crate::theme::write_atomic(user_dir, &path, &doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::scope::{DimensionSelection, Scope};

    fn slots() -> GroupingSlots {
        let mut s = GroupingSlots::default();
        s.set(1, vec!["book".into(), "lhu".into()]);
        s.set(2, vec!["underlying_ref".into()]);
        s
    }

    fn book_scope(book: &str) -> Scope {
        Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec![book.into()],
            }],
            ..Scope::default()
        }
    }

    #[test]
    fn each_mutation_bumps_exactly_its_own_counter() {
        let mut f = Frame::new(slots(), None);
        let v0 = f.versions();

        assert!(f.set_scope(book_scope("BK000")));
        let v1 = f.versions();
        assert_eq!(v1.scope, v0.scope + 1);
        assert_eq!(
            (v1.grouping, v1.as_of, v1.data, v1.config),
            (v0.grouping, v0.as_of, v0.data, v0.config)
        );

        assert!(f.set_active_slot(Some(2)));
        let v2 = f.versions();
        assert_eq!(v2.grouping, v1.grouping + 1);
        assert_eq!(v2.scope, v1.scope);

        assert!(
            f.set_as_of(AsOf::At(
                chrono::DateTime::parse_from_rfc3339("2026-09-03T14:05:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            ))
        );
        assert_eq!(f.versions().as_of, v2.as_of + 1);

        f.note_published();
        assert_eq!(f.versions().data, v2.data + 1);
        f.note_config_reloaded();
        assert_eq!(f.versions().config, v2.config + 1);
    }

    #[test]
    fn an_unchanged_value_bumps_nothing() {
        let mut f = Frame::new(slots(), None);
        let v0 = f.versions();
        assert!(!f.set_scope(Scope::default()));
        assert!(!f.set_active_slot(None));
        assert!(!f.set_as_of(AsOf::Live));
        assert_eq!(f.versions(), v0);
    }

    #[test]
    fn an_empty_slot_cannot_be_activated() {
        let mut f = Frame::new(slots(), None);
        assert!(!f.set_active_slot(Some(5)));
        assert_eq!(f.active_slot(), None);
        assert!(f.set_active_slot(Some(1)));
        assert_eq!(
            f.active_grouping(),
            Some(&["book".to_string(), "lhu".into()][..])
        );
        assert!(f.set_active_slot(None));
        assert_eq!(f.active_grouping(), None);
    }

    #[test]
    fn scope_clear_remembers_one_level_and_undo_restores_it() {
        let mut f = Frame::new(slots(), None);
        f.set_scope(book_scope("BK000"));
        assert!(f.clear_scope());
        assert!(f.scope().is_empty());
        assert!(f.undo_scope());
        assert_eq!(f.scope(), &book_scope("BK000"));
        assert!(!f.undo_scope(), "one level only");
        // Setting a new scope also remembers the previous one.
        f.set_scope(book_scope("BK001"));
        assert!(f.undo_scope());
        assert_eq!(f.scope(), &book_scope("BK000"));
    }

    #[test]
    fn effective_scope_composes_global_and_tile() {
        let mut f = Frame::new(slots(), None);
        f.set_scope(book_scope("BK000"));
        let tile = Scope {
            text: Some("spx".into()),
            ..Scope::default()
        };
        let eff = f.effective_scope(&tile);
        assert_eq!(eff.dimensions, book_scope("BK000").dimensions);
        assert_eq!(eff.text.as_deref(), Some("spx"));
        assert_eq!(f.effective_scope(&Scope::default()), book_scope("BK000"));
    }

    #[test]
    fn replacing_slots_bumps_config_and_grouping_and_drops_a_vanished_active_slot() {
        let mut f = Frame::new(slots(), None);
        f.set_active_slot(Some(2));
        let v = f.versions();
        let mut fewer = GroupingSlots::default();
        fewer.set(1, vec!["book".into()]);
        assert!(f.replace_slots(fewer));
        assert_eq!(f.active_slot(), None, "slot 2 no longer exists");
        assert_eq!(f.versions().config, v.config + 1);
        assert_eq!(f.versions().grouping, v.grouping + 1);
    }

    #[test]
    fn saving_a_slot_updates_memory_and_bumps_grouping_only_when_active() {
        let mut f = Frame::new(slots(), None);
        let v = f.versions();
        assert!(f.save_slot(3, vec!["lhu".into()]).is_ok());
        assert_eq!(f.slots().label(3).as_deref(), Some("lhu"));
        assert_eq!(f.versions().grouping, v.grouping, "not the active slot");
        f.set_active_slot(Some(3));
        let v = f.versions();
        assert!(f.save_slot(3, vec!["book".into()]).is_ok());
        assert_eq!(f.take_pending_persist(), Some((3, vec!["book".into()])));
        assert_eq!(
            f.versions().grouping,
            v.grouping + 1,
            "the active slot changed"
        );
        assert!(f.save_slot(0, vec!["book".into()]).is_err());
        assert!(f.save_slot(3, Vec::new()).is_err());
    }

    #[test]
    fn the_readout_names_the_slot_scope_and_as_of() {
        let mut f = Frame::new(slots(), None);
        let r = f.readout();
        assert_eq!(r.slot, None);
        assert_eq!(r.scope, "");
        assert_eq!(r.as_of, None);

        f.set_active_slot(Some(1));
        f.set_scope(Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into(), "BK001".into(), "BK002".into()],
            }],
            text: Some("spx".into()),
            expression: geode_core::scope::parse_expr("delta01 > 5").ok(),
            ..Scope::default()
        });
        f.set_as_of(AsOf::At(
            chrono::DateTime::parse_from_rfc3339("2026-09-03T14:05:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        ));
        let r = f.readout();
        assert_eq!(r.slot, Some((1, "book / lhu".to_string())));
        assert_eq!(r.scope, "book ∈ {3} · text \"spx\" · expr");
        assert_eq!(r.as_of.as_deref(), Some("2026-09-03 14:05"));
    }

    #[test]
    fn a_slot_is_persisted_as_a_bare_numeric_key() {
        let dir = tempfile::tempdir().unwrap();
        persist_slot_to_user_config(dir.path(), 3, &["lhu".into(), "position_ref".into()]).unwrap();
        let text = std::fs::read_to_string(dir.path().join("groupings.toml")).unwrap();
        let table: toml::Table = text.parse().unwrap();
        assert_eq!(table["config_version"].as_integer(), Some(1));
        assert_eq!(
            table["3"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["lhu", "position_ref"]
        );
        // A second save keeps the first slot.
        persist_slot_to_user_config(dir.path(), 5, &["book".into()]).unwrap();
        let text = std::fs::read_to_string(dir.path().join("groupings.toml")).unwrap();
        let table: toml::Table = text.parse().unwrap();
        assert!(table.contains_key("3") && table.contains_key("5"));
    }
}
