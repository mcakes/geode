//! Calibration engine registry, selected configuration, and worker. The
//! app supplies concrete engines through
//! `geode_core::calibration::Calibrator`, exactly as it supplies vol
//! models and pricers.

pub mod worker;

use geode_core::calibration::Calibrator;
use std::collections::HashMap;
use std::sync::Arc;

pub use worker::{CALIBRATION_BOUND, CalibrationSink, CalibrationWorker};

/// The service's selected calibration engine. With `engine: None`, every
/// request answers [`Self::missing_reason`] instead of preventing startup.
#[derive(Clone, Default)]
pub struct CalibratorConfig {
    pub name: String,
    pub engine: Option<Arc<dyn Calibrator>>,
}

impl CalibratorConfig {
    pub fn with(engine: Arc<dyn Calibrator>) -> CalibratorConfig {
        CalibratorConfig {
            name: engine.name().to_string(),
            engine: Some(engine),
        }
    }

    pub fn missing(name: &str) -> CalibratorConfig {
        CalibratorConfig {
            name: name.to_string(),
            engine: None,
        }
    }

    pub fn missing_reason(&self) -> String {
        if self.name.is_empty() {
            "no calibration engine is configured".to_string()
        } else {
            format!(
                "calibration engine \"{}\" is not built into this binary",
                self.name
            )
        }
    }
}

/// The engines this build has, keyed by [`Calibrator::name`].
#[derive(Default, Clone)]
pub struct CalibratorRegistry {
    engines: HashMap<String, Arc<dyn Calibrator>>,
}

impl CalibratorRegistry {
    pub fn register(&mut self, engine: Arc<dyn Calibrator>) {
        let name = engine.name().to_string();
        if self.engines.insert(name.clone(), engine).is_some() {
            tracing::warn!(
                target: "geode::calibration",
                "calibration engine '{name}' registered twice; the later registration wins"
            );
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Calibrator>> {
        self.engines.get(name).cloned()
    }

    /// Sorted, so a diagnostic listing them reads the same way twice.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.engines.keys().cloned().collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::calibration::{CalibrationError, CalibrationOutcome, CalibrationRequest};

    struct Named(&'static str);
    impl Calibrator for Named {
        fn name(&self) -> &str {
            self.0
        }
        fn run(&self, _: &CalibrationRequest) -> Result<CalibrationOutcome, CalibrationError> {
            Err(CalibrationError("unused".into()))
        }
    }

    #[test]
    fn the_calibrator_registry_answers_by_name_and_lists_sorted() {
        let mut r = CalibratorRegistry::default();
        r.register(Arc::new(Named("vendor")));
        r.register(Arc::new(Named("demo")));
        assert!(r.get("demo").is_some());
        assert!(r.get("nope").is_none());
        assert_eq!(r.names(), vec!["demo", "vendor"]);
    }

    #[test]
    fn a_missing_engine_names_itself_and_an_empty_name_says_none_is_configured() {
        assert_eq!(
            CalibratorConfig::missing("vendor").missing_reason(),
            "calibration engine \"vendor\" is not built into this binary"
        );
        assert_eq!(
            CalibratorConfig::default().missing_reason(),
            "no calibration engine is configured"
        );
        let with = CalibratorConfig::with(Arc::new(Named("demo")));
        assert_eq!(with.name, "demo");
        assert!(with.engine.is_some());
    }
}
