//! The gpui-free half of Geode's composition root. The app and the
//! background collector build their store configuration from these
//! functions, so the two agree on what the store holds.

pub mod demo;
pub mod demo_bus;
pub mod demo_refdb;
pub mod demo_series;
pub mod paths;

pub use paths::{config_dirs, db_path, user_config_dir};

use geode_data::adapter::{AdapterRegistry, ChannelAdapter, ChannelFeed};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// The transport registry both binaries build. Without a demo directory it
/// is empty (real adapters register here when they exist); with one it
/// holds the demo bus, the two demo series transports, the demo reference
/// database and the demo position service, and returns the bus's feed for
/// whoever runs the demo producers.
pub fn adapters(demo_root: Option<&Path>) -> (AdapterRegistry, Option<ChannelFeed>) {
    let Some(root) = demo_root else {
        return (AdapterRegistry::default(), None);
    };
    let (adapter, feed) = ChannelAdapter::new("demo_bus");
    let mut adapters = AdapterRegistry::default();
    adapters.register(adapter);
    // Use the risk generator's seed for both series sources, exercising
    // catalogue and manual-identity discovery.
    adapters.register(demo_series::DemoSeries::new("demo_kdb", 42, true));
    adapters.register(demo_series::DemoSeries::new("demo_rest", 42, false));
    // The demo reference database behind the `refdb` snapshot source.
    adapters.register(demo_refdb::DemoRefDb::new(Duration::ZERO));
    // The demo position service rewrites the risk CSVs the demo source
    // polls; the demo layer's `positions.toml` names it.
    adapters.register(Arc::new(demo::DemoPositions::new(root.join("src"))));
    (adapters, Some(feed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_demo_build_registers_no_adapters() {
        let (registry, feed) = adapters(None);
        assert!(registry.names().is_empty(), "{:?}", registry.names());
        assert!(feed.is_none());
    }

    #[test]
    fn a_demo_build_registers_the_five_demo_transports_and_returns_the_bus_feed() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, feed) = adapters(Some(dir.path()));
        let mut names = registry.names();
        names.sort();
        assert_eq!(
            names,
            [
                "demo_bus",
                "demo_kdb",
                "demo_positions",
                "demo_refdb",
                "demo_rest"
            ]
        );
        assert!(feed.is_some());
    }
}
