//! Action metadata shared by keymaps, palette entries, and dispatch.
//! Modules register action ids and contribute default keymap fragments; bindings
//! are resolved centrally.

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
    /// Retired ids mapped to their current id. Keymap documents written
    /// before a rename still name the old id; [`Self::renamed`] lets the
    /// keymap builder bind them to the current action with a warning.
    renames: BTreeMap<ActionId, ActionId>,
    /// Shared FNV-1a hash-to-id map for crash reporting. The panic hook can
    /// retain this Arc independently of the registry. Registrations update the
    /// shared map; the application registers its actions during startup.
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

    /// Record that `old` was renamed to `new`. The owner of an action
    /// registers its renames beside the action itself. Refuses an `old` that
    /// is itself a registered action, or one already renamed.
    pub fn register_rename(&mut self, old: &str, new: &str) -> Result<(), String> {
        let old = ActionId(old.to_string());
        if self.actions.contains_key(&old) {
            return Err(format!("'{old}' is a registered action, not a retired id"));
        }
        if self.renames.contains_key(&old) {
            return Err(format!("action '{old}' renamed twice"));
        }
        self.renames.insert(old, ActionId(new.to_string()));
        Ok(())
    }

    /// The current id for a retired one, when the current id is registered.
    pub fn renamed(&self, old: &ActionId) -> Option<&ActionId> {
        self.renames.get(old).filter(|new| self.contains(new))
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

    /// Find an action id by its FNV-1a hash, scanning ids in sorted order.
    /// The returned reference borrows this registry rather than a lock guard. On a
    /// hash collision this returns the first sorted id, while [`Self::hash_names`]
    /// retains the most recently registered id for that hash.
    pub fn name_of_hash(&self, h: u64) -> Option<&str> {
        self.actions
            .keys()
            .find(|id| fnv1a(&id.0) == h)
            .map(|id| id.0.as_str())
    }

    /// Share the hash-to-id map with callers that outlive a registry borrow.
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

    /// A retired id resolves only while its successor is registered, and a
    /// live id can never be declared retired.
    #[test]
    fn a_rename_resolves_to_a_registered_successor() {
        let mut reg = ActionRegistry::default();
        reg.register_rename("a::colour", "a::color").unwrap();
        assert_eq!(reg.renamed(&ActionId("a::colour".into())), None);
        reg.register(def("a::color", "Color")).unwrap();
        assert_eq!(
            reg.renamed(&ActionId("a::colour".into())),
            Some(&ActionId("a::color".into()))
        );
        assert!(reg.register_rename("a::colour", "a::other").is_err());
        assert!(reg.register_rename("a::color", "a::x").is_err());
        assert!(!reg.contains(&ActionId("a::colour".into())));
    }

    #[test]
    fn iter_is_sorted_by_id() {
        let mut reg = ActionRegistry::default();
        reg.register(def("b::b", "B")).unwrap();
        reg.register(def("a::a", "A")).unwrap();
        let ids: Vec<_> = reg.iter().map(|d| d.id.to_string()).collect();
        assert_eq!(ids, vec!["a::a", "b::b"]);
    }

    // Registered action ids can be resolved from the hashes recorded in the action tail.
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
