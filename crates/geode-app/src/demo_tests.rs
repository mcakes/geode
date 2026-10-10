//! Tests of the demo transports that need the app: they build the app's
//! data setup, start its bridge or open a gpui window. The transports
//! themselves live in `geode_compose`.

mod demo_config_integration {
    use geode_compose::demo::*;
    use geode_core::config::{Config, ConfigSources};

    /// A registry holding the mock pricer, matching what `main.rs`
    /// builds before calling `data_setup` — without it, the demo
    /// config's implicit `[pricing] adapter = "mock"` default would
    /// resolve to nothing and every fixture below would gain a spurious
    /// "pricer" diagnostic.
    fn test_pricers() -> geode_data::PricerRegistry {
        let mut pricers = geode_data::PricerRegistry::default();
        pricers.register(std::sync::Arc::new(geode_pricing::MockPricer::new()));
        pricers
    }

    /// The same for the default `demo` vol model: an empty registry would
    /// gain a spurious "vol model" diagnostic.
    fn test_vol_models() -> geode_data::VolModelRegistry {
        let mut vol_models = geode_data::VolModelRegistry::default();
        vol_models.register(std::sync::Arc::new(geode_pricing::DemoVolModel));
        vol_models
    }

    /// Registers the demo bus required by the `sophis` egress target,
    /// the demo position service `positions.toml` names, and the demo
    /// reference database the `refdb` source polls, as `main.rs` does.
    ///
    /// Keep the returned feed alive through `data_setup`: egress resolution
    /// upgrades a weak sender reference, and a dropped feed makes the
    /// adapter report that it has no egress side.
    fn test_adapters() -> (
        geode_data::adapter::AdapterRegistry,
        geode_data::adapter::ChannelFeed,
    ) {
        let mut adapters = geode_data::adapter::AdapterRegistry::default();
        let (adapter, feed) = geode_data::adapter::ChannelAdapter::new("demo_bus");
        adapters.register(adapter);
        adapters.register(std::sync::Arc::new(DemoPositions::new(
            "/tmp/geode-demo/100000-42/src".into(),
        )));
        adapters.register(geode_compose::demo_refdb::DemoRefDb::new(
            std::time::Duration::ZERO,
        ));
        (adapters, feed)
    }

    /// `Bridge::positions_configured` is whether `positions.toml`'s service
    /// survived resolution: the demo layer names `demo_positions`, so it is
    /// configured when that adapter is registered and not when it is absent.
    #[gpui::test]
    fn the_bridge_says_whether_a_position_service_is_configured(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(&ConfigSources {
            builtin: layer(&dir.path().join("src")),
            ..ConfigSources::default()
        });
        let (with, _feed) = test_adapters();
        let (bus, _bus_feed) = geode_data::adapter::ChannelAdapter::new("demo_bus");
        let mut without = geode_data::adapter::AdapterRegistry::default();
        without.register(bus);
        for (i, (adapters, expected)) in [(with, true), (without, false)].into_iter().enumerate() {
            let setup = crate::bridge::data_setup(
                &config,
                dir.path().join(format!("geode-{i}.duckdb")),
                adapters,
                test_pricers(),
                test_vol_models(),
            )
            .unwrap();
            let bridge = cx.update(|cx| {
                crate::bridge::start(
                    setup,
                    geode_shell::vimfind::FindStyle::default(),
                    std::time::Duration::from_secs(60),
                    cx,
                )
            });
            assert_eq!(bridge.positions_configured, expected);
            bridge.handle.shutdown();
        }
    }

    /// End to end through the real service: the demo layer's `positions.toml`
    /// resolves `DemoPositions`, `move_lhu` reaches the simulator through the
    /// position worker and is answered `Ok`, and the demo source's next poll
    /// ingests the rewritten partition, so the position's LHU is the target
    /// and only the target (the partition replaced, not added to).
    #[test]
    fn a_move_is_ingested_as_the_new_lhu() {
        use geode_core::positions::MoveLhuParams;
        use geode_core::query::{DistinctParams, QueryKey};
        use geode_core::scope::{DimensionSelection, Scope};
        use geode_data::query::as_of::AsOf;
        use geode_data::{DataEvent, DataService};
        use std::sync::mpsc::Receiver;
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let src = ensure_emitted(dir.path(), 100).unwrap();
        let config = Config::load(&ConfigSources {
            builtin: layer(&src),
            ..ConfigSources::default()
        });
        let mut adapters = geode_data::adapter::AdapterRegistry::default();
        let (bus, _feed) = geode_data::adapter::ChannelAdapter::new("demo_bus");
        adapters.register(bus);
        adapters.register(std::sync::Arc::new(DemoPositions::new(src.clone())));
        let setup = crate::bridge::data_setup(
            &config,
            dir.path().join("geode.duckdb"),
            adapters,
            test_pricers(),
            test_vol_models(),
        )
        .unwrap();
        assert!(setup.config.positions.is_some());
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = DataService::spawn(
            setup.config,
            std::sync::Arc::new(move |e| tx.send(e).is_ok()),
        );

        // A position of one book's file, and an LHU it does not hold.
        let csv = std::fs::read_dir(&src)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.to_string_lossy().ends_with("_BK003.csv"))
            .unwrap();
        let text = std::fs::read_to_string(csv).unwrap();
        let mut lines = text.lines();
        let header: Vec<&str> = lines.next().unwrap().split(',').collect();
        let at = |name| header.iter().position(|c| *c == name).unwrap();
        let first: Vec<&str> = lines.next().unwrap().split(',').collect();
        let p = first[at("PositionRef")].to_string();
        let target = "BK007_LHU2".to_string();
        assert_ne!(first[at("LHU")], target);

        // The LHU values ingested for `p`, polled until `done` holds.
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut tag = 0;
        let mut lhus_until = |rx: &Receiver<DataEvent>, done: &dyn Fn(&[String]) -> bool| loop {
            assert!(
                Instant::now() < deadline,
                "timed out polling the LHU of {p}"
            );
            tag += 1;
            handle
                .distinct(DistinctParams {
                    key: QueryKey(1),
                    tag,
                    column: "lhu".into(),
                    scope: Scope {
                        dimensions: vec![DimensionSelection {
                            column: "position_ref".into(),
                            values: vec![p.clone()],
                        }],
                        ..Scope::default()
                    },
                    as_of: AsOf::Live,
                    dataset: None,
                })
                .unwrap();
            let values = loop {
                match rx.recv_timeout(Duration::from_secs(10)) {
                    Ok(DataEvent::Distinct(o)) if o.tag == tag => break o.values.unwrap(),
                    Ok(_) => continue,
                    Err(e) => panic!("no distinct answer: {e}"),
                }
            };
            let values: Vec<String> = values.into_iter().map(|(v, _)| v).collect();
            if done(&values) {
                break values;
            }
            std::thread::sleep(Duration::from_millis(250));
        };

        let before = lhus_until(&rx, &|v| !v.is_empty());
        assert!(!before.contains(&target), "{before:?}");

        handle
            .move_lhu(MoveLhuParams {
                tag: 1,
                positions: vec![p.clone()],
                lhu: target.clone(),
            })
            .unwrap();
        let outcome = loop {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(DataEvent::Command(o)) => break o,
                Ok(_) => continue,
                Err(e) => panic!("no command answer: {e}"),
            }
        };
        assert_eq!(outcome.result, Ok(()));

        let after = lhus_until(&rx, &|v| v.contains(&target));
        assert_eq!(after, vec![target], "replaced, not added to");
        handle.shutdown();
    }

    /// End to end through the real service: the demo layer's `refdb` source
    /// polls `DemoRefDb` at open, publishes the `underlyings` table, and a
    /// live reference read answers its ten rows keyed by the demo
    /// underlyings.
    #[test]
    fn the_refdb_source_publishes_the_ten_underlyings() {
        use geode_core::query::{QueryKey, ReferenceParams};
        use geode_data::query::as_of::AsOf;
        use geode_data::{DataEvent, DataService};
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let src = ensure_emitted(dir.path(), 100).unwrap();
        let config = Config::load(&ConfigSources {
            builtin: layer(&src),
            ..ConfigSources::default()
        });
        let mut adapters = geode_data::adapter::AdapterRegistry::default();
        let (bus, _feed) = geode_data::adapter::ChannelAdapter::new("demo_bus");
        adapters.register(bus);
        adapters.register(geode_compose::demo_refdb::DemoRefDb::new(Duration::ZERO));
        let setup = crate::bridge::data_setup(
            &config,
            dir.path().join("geode.duckdb"),
            adapters,
            test_pricers(),
            test_vol_models(),
        )
        .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = DataService::spawn(
            setup.config,
            std::sync::Arc::new(move |e| tx.send(e).is_ok()),
        );

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(DataEvent::Published { dataset, .. }) if dataset == "underlyings" => break,
                Ok(_) => continue,
                Err(e) => panic!("underlyings never published: {e}"),
            }
        }
        handle
            .reference(ReferenceParams {
                key: QueryKey(1),
                tag: 1,
                dataset: "underlyings".into(),
                as_of: AsOf::Live,
            })
            .unwrap();
        let table = loop {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(DataEvent::Reference(o)) if o.tag == 1 => break o.table.unwrap().unwrap(),
                Ok(_) => continue,
                Err(e) => panic!("no reference answer: {e}"),
            }
        };
        assert_eq!(table.rows.len(), 10);
        let key = table
            .columns
            .iter()
            .position(|c| c == "underlying_ref")
            .unwrap();
        let mut refs: Vec<String> = table.rows.iter().map(|r| r[key].clone().unwrap()).collect();
        let mut expected = geode_demo_data::demo_underlyings();
        refs.sort();
        expected.sort();
        assert_eq!(refs, expected);
        handle.shutdown();
    }

    /// The demo documents produce a usable data-service configuration
    /// through the normal config loader and setup path. Typed readers must
    /// skip `config_version` headers without reporting invalid entries.
    #[test]
    fn the_demo_layer_produces_a_servable_data_setup() {
        let src = std::path::Path::new("/tmp/geode-demo/100000-42/src");
        let config = Config::load(&ConfigSources {
            builtin: layer(src),
            ..ConfigSources::default()
        });
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        let (adapters, _feed) = test_adapters();
        let setup = crate::bridge::data_setup(
            &config,
            "/tmp/geode-demo/100000-42/geode.duckdb".into(),
            adapters,
            test_pricers(),
            test_vol_models(),
        )
        .expect("datasets + views are both present in the demo layer");
        assert!(setup.diagnostics.is_empty(), "{:?}", setup.diagnostics);
        let names: Vec<&str> = setup.views.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["tree", "wide"]);
        let wide = setup.views.iter().find(|v| v.name == "wide").unwrap();
        assert_eq!(wide.columns.len(), 100, "the wide view has 100 columns");
        // All seven source definitions survive setup: risk CSVs, the CVI,
        // dividend and option-chain subscriptions, the two timeseries fetch
        // adapters, and the reference snapshot.
        assert_eq!(setup.config.sources.len(), 7);
        // Setup must carry the resolved egress target into the service config.
        assert_eq!(setup.config.egress.len(), 1);
        assert_eq!(setup.config.egress[0].name, "sophis");
        // And the resolved position service.
        assert_eq!(
            setup.config.positions,
            Some(geode_core::positions::PositionsSpec {
                adapter: DEMO_POSITIONS.to_string()
            })
        );
        // Each document dataset must match its registered kind's columns.
        // A mismatch would fail the subscribed source's discovery health
        // when the service opens.
        geode_core::document::check_kind_against(
            &geode_documents::CviKind,
            setup.config.schema.dataset("cvi_params").unwrap(),
        )
        .unwrap();
        geode_core::document::check_kind_against(
            &geode_documents::DividendKind,
            setup.config.schema.dataset("dividend_schedule").unwrap(),
        )
        .unwrap();
        geode_core::document::check_kind_against(
            &geode_documents::OptionChainKind,
            setup.config.schema.dataset("option_chain").unwrap(),
        )
        .unwrap();
    }

    /// The default timeseries source must name a declared fetch source.
    /// The shell uses this validation at startup; an invalid default would
    /// warn and require an explicit `@source` for added series.
    #[test]
    fn the_demo_layers_default_timeseries_source_names_one_of_its_own() {
        let src = std::path::Path::new("/tmp/geode-demo/100000-42/src");
        let config = Config::load(&ConfigSources {
            builtin: layer(src),
            ..ConfigSources::default()
        });
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        let diags = geode_shell::series::default_source_diagnostic(&config);
        assert!(diags.is_empty(), "{diags:?}");
    }

    /// The compiled-in schema exposes `currency`, `model_code`, and `expiry`
    /// as categorical columns used by both the picker and dictionary interning.
    #[test]
    fn the_demo_schema_declares_currency_model_code_and_expiry_as_categorical() {
        let src = std::path::Path::new("/tmp/geode-demo/100000-42/src");
        let config = Config::load(&ConfigSources {
            builtin: layer(src),
            ..ConfigSources::default()
        });
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        let (adapters, _feed) = test_adapters();
        let setup = crate::bridge::data_setup(
            &config,
            "/tmp/geode-demo/100000-42/geode.duckdb".into(),
            adapters,
            test_pricers(),
            test_vol_models(),
        )
        .expect("datasets + views are both present in the demo layer");
        assert!(setup.diagnostics.is_empty(), "{:?}", setup.diagnostics);
        let ds = setup
            .config
            .schema
            .dataset("risk_snapshot")
            .expect("risk_snapshot is declared");
        let categorical = ds.categorical_columns();
        for name in ["currency", "model_code", "expiry"] {
            assert!(
                categorical.contains(&name),
                "'{name}' must be categorical: {categorical:?}"
            );
        }
    }
}

mod demo_bus {
    use chrono::NaiveDate;
    use geode_compose::demo_bus::*;
    use geode_core::document::DocumentRows;
    use geode_data::adapter::{ChannelAdapter, MessageSink};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// The first event `pick` accepts, within one overall deadline. A
    /// per-receive timeout alone never fires while unrelated events (the
    /// demo layer's polled sources report health every few seconds) keep
    /// arriving, so a lost event would hang the test instead of failing it.
    fn next_event<T>(
        rx: &std::sync::mpsc::Receiver<geode_data::DataEvent>,
        what: &str,
        mut pick: impl FnMut(geode_data::DataEvent) -> Option<T>,
    ) -> T {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = rx
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("{what} arrives within 15 s"));
            if let Some(found) = pick(event) {
                return found;
            }
        }
    }

    /// The dividend panel the composition root loads declares its status
    /// choices once, in its TOML; the parser and the demo generator must
    /// offer exactly those. Only the composition root sees the loaded panel
    /// and the parser together, without a feature-to-parser dependency.
    #[test]
    fn the_dividend_panel_specs_status_vocabulary_matches_the_dividend_kind() {
        let dir = tempfile::tempdir().unwrap();
        let config = geode_core::config::Config::load(&geode_core::config::ConfigSources {
            builtin: crate::builtin_layer(Some(dir.path())),
            ..Default::default()
        });
        let setup = crate::bridge::data_setup(
            &config,
            dir.path().join("geode.duckdb"),
            geode_data::adapter::AdapterRegistry::default(),
            geode_data::PricerRegistry::default(),
            geode_data::VolModelRegistry::default(),
        )
        .expect("the demo layer declares datasets and views");
        let dividend = setup
            .panels
            .iter()
            .find(|p| p.kind == "dividend")
            .expect("the builtin dividend panel is accepted");
        let choices = dividend
            .value_column("status")
            .and_then(|c| c.choices.as_deref())
            .expect("status declares its choices");
        assert_eq!(choices, geode_documents::dividend::STATUSES);
        assert_eq!(choices, geode_demo_data::documents::dividend::STATUSES);
    }

    /// Upload bytes must reach the subscribed source and return through an
    /// ordinary document query. The demo `sophis` target routes the upload to
    /// `marketdata/dividend/XYZ/NOTIFY`; the dividend subscription parses and stores it.
    /// Each awaited event has one overall deadline, so a silent pipeline fails the test.
    #[test]
    fn an_uploaded_dividend_document_echoes_through_the_real_data_service() {
        use geode_core::config::{Config, ConfigSources};
        use geode_core::document::{Column, DocumentRows, Value};
        use geode_core::query::{DocumentParams, QueryKey};
        use geode_data::adapter::AdapterRegistry;
        use geode_data::egress::UploadParams;
        use geode_data::query::as_of::AsOf;
        use geode_data::{DataEvent, DataService, PricerRegistry, VolModelRegistry};

        // An empty directory rather than a nonexistent path: the demo
        // layer's own `[demo]` csv_dir source polls it, and this test has
        // no interest in that source's health, only that `DataService::open`
        // does not fail to construct over it.
        let src_dir = tempfile::tempdir().unwrap();
        let config = Config::load(&ConfigSources {
            builtin: geode_compose::demo::layer(src_dir.path()),
            ..ConfigSources::default()
        });
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);

        let mut adapters = AdapterRegistry::default();
        let (bus, _feed) = ChannelAdapter::new("demo_bus");
        adapters.register(bus);
        // The demo layer's `positions.toml` names it, as `geode_compose::adapters` registers.
        adapters.register(Arc::new(geode_compose::demo::DemoPositions::new(
            src_dir.path().to_path_buf(),
        )));
        let mut pricers = PricerRegistry::default();
        pricers.register(Arc::new(geode_pricing::MockPricer::new()));
        let mut vol_models = VolModelRegistry::default();
        vol_models.register(Arc::new(geode_pricing::DemoVolModel));

        let db_dir = tempfile::tempdir().unwrap();
        let setup = crate::bridge::data_setup(
            &config,
            db_dir.path().join("geode.duckdb"),
            adapters,
            pricers,
            vol_models,
        )
        .expect("the demo layer carries both a datasets and a views document");
        assert!(setup.diagnostics.is_empty(), "{:?}", setup.diagnostics);
        assert_eq!(setup.config.egress.len(), 1);
        assert_eq!(setup.config.egress[0].name, "sophis");

        let (service, rx) = DataService::open_channel(setup.config)
            .expect("the demo schema opens cleanly against a fresh database");

        // Two rows share an ex date to exercise ordinal ID minting through
        // the full pipeline. Placeholder draft labels are omitted by the
        // writer; the parser assigns IDs from ex dates and row order.
        let ex1 = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        let ex2 = NaiveDate::from_ymd_opt(2027, 1, 5).unwrap();
        let uploaded = DocumentRows {
            key: vec!["XYZ".to_string()],
            attributes: vec![
                ("currency".to_string(), Value::Utf8("USD".to_string())),
                ("schedule_date".to_string(), Value::Date(ex1)),
            ],
            axes: vec![(
                "dividend_id".to_string(),
                Column::Utf8(vec!["new-1".into(), "new-2".into(), "new-3".into()]),
            )],
            values: vec![
                ("ex_date".to_string(), Column::Date(vec![ex1, ex1, ex2])),
                (
                    "announced_date".to_string(),
                    Column::Date(vec![ex1, ex1, ex2]),
                ),
                ("pay_date".to_string(), Column::Date(vec![ex1, ex1, ex2])),
                ("amount".to_string(), Column::F64(vec![1.0, 2.0, 3.0])),
                (
                    "status".to_string(),
                    Column::Utf8(vec![
                        "declared".into(),
                        "declared".into(),
                        "estimated".into(),
                    ]),
                ),
            ],
        };

        service.upload(UploadParams {
            key: QueryKey(1),
            tag: 1,
            target: "sophis".to_string(),
            document: "dividend_schedule".to_string(),
            rows: uploaded,
        });

        let upload_result = next_event(&rx, "an upload outcome", |e| match e {
            DataEvent::Upload(outcome) => Some(outcome.result),
            _ => None,
        });
        assert_eq!(upload_result, Ok(()));

        let (dataset, batch) = next_event(&rx, "a dividend publish", |e| match e {
            DataEvent::Published { dataset, batch, .. } if dataset == "dividend_schedule" => {
                Some((dataset, batch))
            }
            _ => None,
        });
        assert_eq!(
            (dataset.as_str(), batch.as_str()),
            ("dividend_schedule", "XYZ")
        );

        service
            .document(&DocumentParams {
                key: QueryKey(2),
                tag: 1,
                submitted: Instant::now(),
                dataset: "dividend_schedule".to_string(),
                document_key: vec!["XYZ".to_string()],
                as_of: AsOf::Live,
            })
            .expect("the document request is admitted");
        let snap = next_event(&rx, "a query outcome", |e| match e {
            DataEvent::Query(outcome) if outcome.key == QueryKey(2) => {
                Some(outcome.snapshot.expect("the document reads back"))
            }
            _ => None,
        });

        let expected_ids = geode_documents::dividend::mint_ids(&[ex1, ex1, ex2]);
        assert!(
            expected_ids.iter().all(|id| !id.starts_with("new-")),
            "{expected_ids:?}"
        );
        let ex_dates = [ex1, ex1, ex2];
        let amounts = [1.0, 2.0, 3.0];
        let statuses = ["declared", "declared", "estimated"];
        assert_eq!(snap.rows(), 3);
        for i in 0..3 {
            assert_eq!(
                snap.text_value("dividend_id", i),
                Some(expected_ids[i].as_str()),
                "row {i}: the id is re-minted from the ex date, not carried from the upload"
            );
            assert_eq!(
                snap.display_value("ex_date", i),
                Some(ex_dates[i].format("%Y-%m-%d").to_string())
            );
            assert_eq!(snap.f64_value("amount", i), Some(amounts[i]));
            assert_eq!(snap.text_value("status", i), Some(statuses[i]));
        }

        service.shutdown();
    }
    /// The store sorts document rows by their axes while uploads use painted
    /// order. An inserted dividend with a later ex date can move on readback;
    /// the panel's echo comparison must still confirm the same contents.
    #[test]
    fn an_out_of_order_insert_echoes_back_as_confirmed_through_the_real_store() {
        use geode_core::config::{Config, ConfigSources};
        use geode_core::document::{Column, DocumentRows, Value};
        use geode_core::query::{DocumentParams, QueryKey};
        use geode_core::snapshot::Snapshot;
        use geode_data::adapter::AdapterRegistry;
        use geode_data::egress::UploadParams;
        use geode_data::query::as_of::AsOf;
        use geode_data::{DataEvent, DataService, PricerRegistry, VolModelRegistry};
        use geode_marketdata::core::upload::{assemble, echo_differs};
        use geode_marketdata::core::{Draft, MatrixIndex, builtin_panel};
        use std::sync::mpsc::Receiver;

        let dividend = builtin_panel("dividend");
        let src_dir = tempfile::tempdir().unwrap();
        let config = Config::load(&ConfigSources {
            builtin: geode_compose::demo::layer(src_dir.path()),
            ..ConfigSources::default()
        });
        let mut adapters = AdapterRegistry::default();
        let (bus, _feed) = ChannelAdapter::new("demo_bus");
        adapters.register(bus);
        let mut pricers = PricerRegistry::default();
        pricers.register(Arc::new(geode_pricing::MockPricer::new()));
        let mut vol_models = VolModelRegistry::default();
        vol_models.register(Arc::new(geode_pricing::DemoVolModel));
        let db_dir = tempfile::tempdir().unwrap();
        let setup = crate::bridge::data_setup(
            &config,
            db_dir.path().join("geode.duckdb"),
            adapters,
            pricers,
            vol_models,
        )
        .expect("the demo layer opens");
        let (service, rx) = DataService::open_channel(setup.config).expect("the store opens");

        // Upload, wait for its `Ok` and the publish it echoes as, then
        // read the document back through an ordinary document request.
        let round_trip = |service: &DataService,
                          rx: &Receiver<DataEvent>,
                          tag: u64,
                          rows: DocumentRows|
         -> Arc<Snapshot> {
            service.upload(UploadParams {
                key: QueryKey(1),
                tag,
                target: "sophis".to_string(),
                document: "dividend_schedule".to_string(),
                rows,
            });
            let (mut ok, mut published) = (None, false);
            next_event(rx, "the upload outcome and its echo", |e| {
                match e {
                    DataEvent::Upload(o) if o.tag == tag => ok = Some(o.result),
                    DataEvent::Published { dataset, .. } if dataset == "dividend_schedule" => {
                        published = true
                    }
                    _ => {}
                }
                (ok.is_some() && published).then_some(())
            });
            assert_eq!(ok, Some(Ok(())));
            service
                .document(&DocumentParams {
                    key: QueryKey(2),
                    tag,
                    submitted: Instant::now(),
                    dataset: "dividend_schedule".to_string(),
                    document_key: vec!["XYZ".to_string()],
                    as_of: AsOf::Live,
                })
                .expect("the document request is admitted");
            next_event(rx, "a query outcome", |e| match e {
                DataEvent::Query(o) if o.key == QueryKey(2) && o.tag == tag => {
                    Some(o.snapshot.expect("the document reads back"))
                }
                _ => None,
            })
        };

        let d = |m, day| NaiveDate::from_ymd_opt(2026, m, day).unwrap();
        let exes = vec![d(10, 1), d(11, 2), d(12, 3)];
        let first = DocumentRows {
            key: vec!["XYZ".to_string()],
            attributes: vec![
                ("currency".to_string(), Value::Utf8("USD".to_string())),
                ("schedule_date".to_string(), Value::Date(d(9, 1))),
            ],
            axes: vec![(
                "dividend_id".to_string(),
                Column::Utf8(vec!["new-1".into(), "new-2".into(), "new-3".into()]),
            )],
            values: vec![
                ("ex_date".to_string(), Column::Date(exes.clone())),
                ("announced_date".to_string(), Column::Date(exes.clone())),
                ("pay_date".to_string(), Column::Date(exes.clone())),
                ("amount".to_string(), Column::F64(vec![1.0, 2.0, 3.0])),
                (
                    "status".to_string(),
                    Column::Utf8(vec!["declared".into(); 3]),
                ),
            ],
        };
        let base = round_trip(&service, &rx, 1, first);
        assert_eq!(base.rows(), 3);

        // The panel's own route: a clean model of the base, an inserted
        // row under the FIRST document row carrying the LATEST ex date,
        // then the painted model and the assembled upload.
        let clean = MatrixIndex::build(&base, &dividend, &Draft::default()).unwrap();
        let first_label = clean.label(0).expect("a row").to_string();
        let mut draft = Draft::default();
        let label = draft.mint_label(|l| clean.row_of(l).is_some());
        // Stamp the insert against the delivered generation the clean model
        // was built from, exactly as the panel does.
        let stamp = clean.base.clone().unwrap_or_default();
        draft.insert_row(label.clone(), Some(first_label), &stamp);
        let late = NaiveDate::from_ymd_opt(2027, 3, 19).unwrap();
        for (column, value) in [
            ("ex", Value::Date(late)),
            ("announced", Value::Date(late)),
            ("pay", Value::Date(late)),
            ("amount", Value::F64(0.75)),
            ("status", Value::Utf8("estimated".into())),
        ] {
            assert!(draft.set_row_cell(&label, column, value), "{column}");
        }
        let painted = MatrixIndex::build(&base, &dividend, &draft).unwrap();
        let sent = assemble(&base, &dividend, &painted, &draft).expect("assembles");
        let Column::Date(sent_ex) = &sent.values[0].1 else {
            panic!("ex_date is a date column");
        };
        assert_eq!(
            sent_ex[1], late,
            "the insert sits second, out of date order"
        );

        let echoed = round_trip(&service, &rx, 2, sent.clone());
        let clean = MatrixIndex::build(&echoed, &dividend, &Draft::default()).unwrap();
        let delivered =
            assemble(&echoed, &dividend, &clean, &Draft::default()).expect("the echo assembles");
        let Column::Date(echo_ex) = &delivered.values[0].1 else {
            panic!("ex_date is a date column");
        };
        assert_ne!(
            sent_ex, echo_ex,
            "the store reorders the rows — otherwise this test proves nothing"
        );
        assert_eq!(echo_differs(&dividend, &sent, &delivered), 0);

        service.shutdown();
    }

    /// The demo producer's NOTIFY topic, the demo layer's subscription and
    /// the bus's last value carry recovery end to end: a dividend schedule
    /// the producer published while the app was closed goes live at the
    /// next open with no further publish. A producer topic off the
    /// subscribed pattern would never be recorded, and the reopened source
    /// would serve the stale schedule until the producer's next tick.
    #[test]
    fn a_dividend_published_while_closed_is_recovered_at_the_next_open() {
        use geode_core::config::{Config, ConfigSources};
        use geode_core::query::{DocumentParams, QueryKey};
        use geode_data::adapter::AdapterRegistry;
        use geode_data::query::as_of::AsOf;
        use geode_data::{DataEvent, DataService, PricerRegistry, VolModelRegistry};
        use std::sync::Mutex;
        use std::sync::mpsc::Receiver;

        let src_dir = tempfile::tempdir().unwrap();
        let db_dir = tempfile::tempdir().unwrap();
        let db = db_dir.path().join("geode.duckdb");
        let config = Config::load(&ConfigSources {
            builtin: geode_compose::demo::layer(src_dir.path()),
            ..ConfigSources::default()
        });
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        // One bus outlives both service runs, as the simulated broker
        // outlives an app restart; it keeps the last message per topic.
        let (bus, feed) = ChannelAdapter::new("demo_bus");
        let mut adapters = AdapterRegistry::default();
        adapters.register(bus);
        adapters.register(Arc::new(geode_compose::demo::DemoPositions::new(
            src_dir.path().to_path_buf(),
        )));
        let open = |adapters: AdapterRegistry| {
            let mut pricers = PricerRegistry::default();
            pricers.register(Arc::new(geode_pricing::MockPricer::new()));
            let mut vol_models = VolModelRegistry::default();
            vol_models.register(Arc::new(geode_pricing::DemoVolModel));
            let setup =
                crate::bridge::data_setup(&config, db.clone(), adapters, pricers, vol_models)
                    .expect("the demo layer opens");
            DataService::open_channel(setup.config).expect("the store opens")
        };
        let timeout = Duration::from_secs(15);
        let wait_published = |rx: &Receiver<DataEvent>| {
            next_event(rx, "a dividend publish", |e| match e {
                DataEvent::Published { dataset, batch, .. } if dataset == "dividend_schedule" => {
                    assert_eq!(batch, "SPX");
                    Some(())
                }
                _ => None,
            })
        };
        let live_amounts = |service: &DataService, rx: &Receiver<DataEvent>, tag: u64| {
            service
                .document(&DocumentParams {
                    key: QueryKey(2),
                    tag,
                    submitted: Instant::now(),
                    dataset: "dividend_schedule".to_string(),
                    document_key: vec!["SPX".to_string()],
                    as_of: AsOf::Live,
                })
                .expect("the document request is admitted");
            let snap = next_event(rx, "a query outcome", |e| match e {
                DataEvent::Query(o) if o.key == QueryKey(2) && o.tag == tag => {
                    Some(o.snapshot.expect("the document reads back"))
                }
                _ => None,
            });
            let mut amounts: Vec<f64> = (0..snap.rows())
                .map(|i| snap.f64_value("amount", i).expect("an amount"))
                .collect();
            amounts.sort_by(f64::total_cmp);
            amounts
        };

        // The demo's own dividend producer, publishing through the bus's
        // publish path; each published document is kept for comparison.
        let anchor = NaiveDate::from_ymd_opt(2026, 9, 12).unwrap();
        let mut producer = demo_producers(vec!["SPX".to_string()], anchor)
            .into_iter()
            .find(|p| p.topic_prefix == "marketdata/dividend/")
            .expect("the demo publishes dividends");
        let sent: Arc<Mutex<Vec<DocumentRows>>> = Arc::default();
        let mut next: Box<NextDocument> = Box::new({
            let sent = Arc::clone(&sent);
            move |key| {
                let rows = (producer.next)(key);
                sent.lock().unwrap().extend(rows.clone());
                rows
            }
        });
        let mut warned = false;
        let sent_amounts = |i: usize| {
            let sent = sent.lock().unwrap();
            let geode_core::document::Column::F64(amounts) = &sent[i]
                .values
                .iter()
                .find(|(name, _)| name == "amount")
                .expect("an amount column")
                .1
            else {
                panic!("amount is a float column");
            };
            let mut amounts = amounts.clone();
            amounts.sort_by(f64::total_cmp);
            amounts
        };

        let (service, rx) = open(adapters.clone());
        publish_one(
            &feed,
            &producer.kind,
            producer.topic_prefix,
            &mut next,
            "SPX",
            &mut warned,
        );
        wait_published(&rx);
        assert_eq!(live_amounts(&service, &rx, 1), sent_amounts(0));
        service.shutdown();
        drop(service);

        // A newer schedule while the app is closed. The throwaway
        // subscriber proves the dispatcher took it, which records the last
        // message per topic before any delivery.
        let mut watcher = adapters.get("demo_bus").unwrap().subscription().unwrap();
        let (sink, seen) = MessageSink::bounded(4);
        watcher
            .subscribe(&["marketdata/>".to_string()], sink, Arc::new(|_| {}))
            .unwrap();
        publish_one(
            &feed,
            &producer.kind,
            producer.topic_prefix,
            &mut next,
            "SPX",
            &mut warned,
        );
        seen.recv_timeout(timeout)
            .expect("the bus dispatched the newer schedule");
        watcher.unsubscribe();
        assert_ne!(
            sent_amounts(0),
            sent_amounts(1),
            "the producer's second schedule differs — otherwise recovery is unobservable"
        );

        // Reopened on the same store with nothing published: SPX can only
        // go live with the newer schedule through recovery.
        let (service, rx) = open(adapters);
        wait_published(&rx);
        assert_eq!(live_amounts(&service, &rx, 2), sent_amounts(1));
        service.shutdown();
    }
}
