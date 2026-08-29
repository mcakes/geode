//! The shared action registry (spec §3.2, §9.1): modules declare actions
//! here; the keymap maps keys to action ids; the palette lists them. Modules
//! never bind keys directly.

use std::collections::BTreeMap;
use std::fmt;

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
}

impl ActionRegistry {
    pub fn register(&mut self, def: ActionDef) -> Result<(), String> {
        if self.actions.contains_key(&def.id) {
            return Err(format!("action '{}' registered twice", def.id));
        }
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
}
