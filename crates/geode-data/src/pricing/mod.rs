//! The pricing seam's data-tier side (line-pricer spec §5.3, §5.5): which
//! `Pricer` this build has, and the worker that runs it.

pub mod worker;

use geode_core::pricing::Pricer;
use std::collections::HashMap;
use std::sync::Arc;

pub use worker::{PRICE_BOUND, PriceSink, PricingWorker};

/// The pricer the service runs. `pricer: None` is a configured name this
/// build cannot serve — every line answers [`PricerConfig::missing_reason`],
/// never a startup failure (spec §4, roadmap ruling 3 applied to a
/// library).
#[derive(Clone, Default)]
pub struct PricerConfig {
    pub name: String,
    pub pricer: Option<Arc<dyn Pricer>>,
}

impl PricerConfig {
    pub fn with(pricer: Arc<dyn Pricer>) -> PricerConfig {
        PricerConfig {
            name: pricer.name().to_string(),
            pricer: Some(pricer),
        }
    }

    pub fn missing(name: &str) -> PricerConfig {
        PricerConfig {
            name: name.to_string(),
            pricer: None,
        }
    }

    pub fn missing_reason(&self) -> String {
        if self.name.is_empty() {
            "no pricer is configured".to_string()
        } else {
            format!("pricer \"{}\" is not built into this binary", self.name)
        }
    }
}

/// The pricers this build has, keyed by [`Pricer::name`]. Shaped exactly
/// like [`crate::adapter::AdapterRegistry`]: `geode-app` is the one crate
/// that knows what was compiled in.
#[derive(Default, Clone)]
pub struct PricerRegistry {
    pricers: HashMap<String, Arc<dyn Pricer>>,
}

impl PricerRegistry {
    pub fn register(&mut self, pricer: Arc<dyn Pricer>) {
        let name = pricer.name().to_string();
        if self.pricers.insert(name.clone(), pricer).is_some() {
            tracing::warn!(
                target: "geode::pricing",
                "pricer '{name}' registered twice; the later registration wins"
            );
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Pricer>> {
        self.pricers.get(name).cloned()
    }

    /// Sorted, so a diagnostic listing them reads the same way twice.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.pricers.keys().cloned().collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::pricing::{PriceRequest, PriceResult, PricingError};

    struct Named(&'static str);
    impl Pricer for Named {
        fn name(&self) -> &str {
            self.0
        }
        fn price(&self, _: &PriceRequest) -> Result<PriceResult, PricingError> {
            Err(PricingError("unused".into()))
        }
    }

    #[test]
    fn the_registry_answers_by_name_and_lists_sorted() {
        let mut r = PricerRegistry::default();
        r.register(Arc::new(Named("vendor")));
        r.register(Arc::new(Named("mock")));
        assert!(r.get("mock").is_some());
        assert!(r.get("nope").is_none());
        assert_eq!(r.names(), vec!["mock", "vendor"]);
    }

    #[test]
    fn a_missing_pricer_names_itself_and_an_empty_name_says_none_is_configured() {
        assert_eq!(
            PricerConfig::missing("vendor").missing_reason(),
            "pricer \"vendor\" is not built into this binary"
        );
        assert_eq!(
            PricerConfig::default().missing_reason(),
            "no pricer is configured"
        );
        let with = PricerConfig::with(Arc::new(Named("mock")));
        assert_eq!(with.name, "mock");
        assert!(with.pricer.is_some());
    }
}
