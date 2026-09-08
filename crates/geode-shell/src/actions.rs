//! The shared action registry (spec §3.2, §9.1): modules declare actions
//! here; the keymap maps keys to action ids; the palette lists them. Modules
//! never bind keys directly.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Arc, RwLock};

use crate::diagnostics::fnv1a;

/// Stable action identifier, `module::action` by convention
/// (e.g. `workspace::focus_left`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActionId(pub String);

impl fmt::Display for ActionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone)]
pub struct ActionDef {
    pub id: ActionId,
    /// Human-readable, palette-facing: "Focus left".
    pub title: String,
    /// Palette grouping: "Workspace".
    pub category: String,
}

#[derive(Debug, Default)]
pub struct ActionRegistry {
    actions: BTreeMap<ActionId, ActionDef>,
    /// FNV-1a hash → action id, filled at [`Self::register`]. Shared (an
    /// `Arc`, not a plain map) so [`Self::hash_names`] can hand out a
    /// clone that stays live and current across later registrations
    /// (e.g. a config reload's `register_pick_actions`/
    /// `register_scope_actions`) — the crash hook (Phase 4b Task 6,
    /// `geode_app::crash::install_panic_hook`) is installed once, as a
    /// 'static closure, with no live reference to this registry, and
    /// resolves `ActionTail`'s hashes through that clone instead.
    hashes: Arc<RwLock<HashMap<u64, String>>>,
}

impl ActionRegistry {
    pub fn register(&mut self, def: ActionDef) -> Result<(), String> {
        if self.actions.contains_key(&def.id) {
            return Err(format!("action '{}' registered twice", def.id));
        }
        self.hashes
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(fnv1a(&def.id.0), def.id.0.clone());
        self.actions.insert(def.id.clone(), def);
        Ok(())
    }

    pub fn get(&self, id: &ActionId) -> Option<&ActionDef> {
        self.actions.get(id)
    }

    pub fn contains(&self, id: &ActionId) -> bool {
        self.actions.contains_key(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ActionDef> {
        self.actions.values()
    }

    /// The registered action whose id hashes (FNV-1a) to `h`, if any.
    /// Scans `self.actions` (already the single source of truth for
    /// every registered id) rather than reading through `hashes` — that
    /// field is `Arc<RwLock<_>>` for [`Self::hash_names`]'s sake, and a
    /// method borrowing `&self` cannot hand back a `&str` tied to a lock
    /// guard that does not outlive the call.
    pub fn name_of_hash(&self, h: u64) -> Option<&str> {
        self.actions
            .keys()
            .find(|id| fnv1a(&id.0) == h)
            .map(|id| id.0.as_str())
    }

    /// A clone of the shared hash → id map, for a caller (`geode-app`'s
    /// panic hook) that needs to resolve hashes from outside any live
    /// reference to this registry. See the `hashes` field's own doc
    /// comment.
    pub fn hash_names(&self) -> Arc<RwLock<HashMap<u64, String>>> {
        self.hashes.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(id: &str, title: &str) -> ActionDef {
        ActionDef {
            id: ActionId(id.to_string()),
            title: title.to_string(),
            category: "Test".to_string(),
        }
    }

    #[test]
    fn register_and_get() {
        let mut reg = ActionRegistry::default();
        reg.register(def("workspace::focus_left", "Focus left"))
            .unwrap();
        assert!(reg.contains(&ActionId("workspace::focus_left".into())));
        assert_eq!(
            reg.get(&ActionId("workspace::focus_left".into()))
                .unwrap()
                .title,
            "Focus left"
        );
        assert!(!reg.contains(&ActionId("workspace::nope".into())));
    }

    #[test]
    fn duplicate_registration_is_an_error() {
        let mut reg = ActionRegistry::default();
        reg.register(def("a::b", "First")).unwrap();
        let err = reg.register(def("a::b", "Second")).unwrap_err();
        assert!(err.contains("a::b"));
        assert_eq!(reg.get(&ActionId("a::b".into())).unwrap().title, "First");
    }

    #[test]
    fn iter_is_sorted_by_id() {
        let mut reg = ActionRegistry::default();
        reg.register(def("b::b", "B")).unwrap();
        reg.register(def("a::a", "A")).unwrap();
        let ids: Vec<_> = reg.iter().map(|d| d.id.to_string()).collect();
        assert_eq!(ids, vec!["a::a", "b::b"]);
    }

    // Phase 4b Task 6: the crash hook resolves `ActionTail`'s FNV-1a
    // hashes back to action ids through this — see `hashes`' own doc
    // comment.
    #[test]
    fn registering_then_looking_up_by_hash_returns_the_id() {
        let mut reg = ActionRegistry::default();
        reg.register(def("workspace::focus_left", "Focus left"))
            .unwrap();
        assert_eq!(
            reg.name_of_hash(fnv1a("workspace::focus_left")),
            Some("workspace::focus_left")
        );
        assert_eq!(reg.name_of_hash(fnv1a("nothing::registered")), None);
    }

    #[test]
    fn hash_names_reflects_registrations_made_after_the_clone_was_taken() {
        let mut reg = ActionRegistry::default();
        let names = reg.hash_names();
        assert!(
            names
                .read()
                .unwrap()
                .get(&fnv1a("workspace::focus_left"))
                .is_none()
        );
        reg.register(def("workspace::focus_left", "Focus left"))
            .unwrap();
        assert_eq!(
            names.read().unwrap().get(&fnv1a("workspace::focus_left")),
            Some(&"workspace::focus_left".to_string())
        );
    }
}
