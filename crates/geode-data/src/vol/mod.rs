//! Vol model registry, selected configuration, and worker. The app
//! supplies concrete models through `geode_core::vol::VolModel`, exactly
//! as it supplies pricers.

pub mod worker;

use geode_core::vol::VolModel;
use std::collections::HashMap;
use std::sync::Arc;

pub use worker::{VOL_BOUND, VolSink, VolWorker, evaluate};

/// The service's selected vol model. With `model: None`, every job of a
/// batch returns [`Self::missing_reason`] instead of preventing startup.
#[derive(Clone, Default)]
pub struct VolConfig {
    pub name: String,
    pub model: Option<Arc<dyn VolModel>>,
}

impl VolConfig {
    pub fn with(model: Arc<dyn VolModel>) -> VolConfig {
        VolConfig {
            name: model.name().to_string(),
            model: Some(model),
        }
    }

    pub fn missing(name: &str) -> VolConfig {
        VolConfig {
            name: name.to_string(),
            model: None,
        }
    }

    pub fn missing_reason(&self) -> String {
        if self.name.is_empty() {
            "no vol model is configured".to_string()
        } else {
            format!("vol model \"{}\" is not built into this binary", self.name)
        }
    }
}

/// The models this build has, keyed by [`VolModel::name`].
#[derive(Default, Clone)]
pub struct VolModelRegistry {
    models: HashMap<String, Arc<dyn VolModel>>,
}

impl VolModelRegistry {
    pub fn register(&mut self, model: Arc<dyn VolModel>) {
        let name = model.name().to_string();
        if self.models.insert(name.clone(), model).is_some() {
            tracing::warn!(
                target: "geode::vol",
                "vol model '{name}' registered twice; the later registration wins"
            );
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn VolModel>> {
        self.models.get(name).cloned()
    }

    /// Sorted, so a diagnostic listing them reads the same way twice.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.models.keys().cloned().collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::document::DocumentRows;
    use geode_core::vol::{MapRequest, SliceRequest, SliceResult, VolError};

    struct Named(&'static str);
    impl VolModel for Named {
        fn name(&self) -> &str {
            self.0
        }
        fn kind(&self) -> &str {
            "cvi_params"
        }
        fn slice(&self, _: &DocumentRows, _: &SliceRequest) -> Result<SliceResult, VolError> {
            Err(VolError("unused".into()))
        }
        fn coordinates(&self, _: &MapRequest) -> Result<Vec<f64>, VolError> {
            Err(VolError("unused".into()))
        }
    }

    #[test]
    fn the_vol_registry_answers_by_name_and_lists_sorted() {
        let mut r = VolModelRegistry::default();
        r.register(Arc::new(Named("vendor")));
        r.register(Arc::new(Named("demo")));
        assert!(r.get("demo").is_some());
        assert!(r.get("nope").is_none());
        assert_eq!(r.names(), vec!["demo", "vendor"]);
    }

    #[test]
    fn a_missing_vol_model_names_itself_and_an_empty_name_says_none_is_configured() {
        assert_eq!(
            VolConfig::missing("vendor").missing_reason(),
            "vol model \"vendor\" is not built into this binary"
        );
        assert_eq!(
            VolConfig::default().missing_reason(),
            "no vol model is configured"
        );
        let with = VolConfig::with(Arc::new(Named("demo")));
        assert_eq!(with.name, "demo");
        assert!(with.model.is_some());
    }
}
