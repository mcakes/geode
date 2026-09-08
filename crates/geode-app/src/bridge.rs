//! Where shell and data meet (Phase 3 spec §5.1, §9.1): builds the
//! service config from the layered docs, spawns the service behind a
//! `DataHandle`, drains its event channel into the shell on a task that
//! wakes on arrival, and forwards config reloads back to the data thread.

use geode_blotter::BlotterFactory;
use geode_core::config::{Config, Diagnostic};
use geode_core::dimensions::DerivedDimensions;
use geode_core::query::DistinctOutcome;
use geode_core::schema::SchemaSpec;
use geode_core::view::ViewSpec;
use geode_data::source::SourceSpec;
use geode_data::{DataEvent, DataHandle, DataService, DataServiceConfig, EventSink};
use geode_shell::shell::{ShellEvent, ShellView};
use geode_shell::vimfind::FindStyle;
use gpui::{App, AsyncApp, WindowHandle};
use gpui_component::Root;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Outbound events queued before the sink refuses (§7.3). Tiles × a
/// small burst; a full channel is counted by the bridge's own
/// `dropped` counter. The sink handed to `DataService::spawn` must
/// `try_send`, never block, and a refusal is counted rather than
/// silently lost — surfaced through `ShellView::set_data_status`.
const EVENT_BOUND: usize = 256;

pub struct DataSetup {
    pub config: DataServiceConfig,
    pub views: Vec<ViewSpec>,
    /// The same `DerivedDimensions` already folded into `config.dimensions`
    /// for the service, carried alongside it too so a caller with a
    /// `DataSetup` in hand (rather than reaching into `config`) has it
    /// directly — mirrors `views` being both `config.views` and its own
    /// field for the same reason. `start` now reads this too (Phase 4a
    /// §3.7): the blotter factory validates `:filter`/`:scope` against
    /// the same schema and dimensions the service itself runs on.
    pub dimensions: DerivedDimensions,
    pub diagnostics: Vec<Diagnostic>,
}

/// `None` when there is nothing to serve: no datasets or no views.
pub fn data_setup(config: &Config, db_path: PathBuf) -> Option<DataSetup> {
    let datasets = config.doc("datasets")?;
    let views_doc = config.doc("views")?;
    let mut diagnostics = Vec::new();
    let (schema, d) = SchemaSpec::from_doc(datasets);
    diagnostics.extend(d);
    let (views, d) = ViewSpec::from_doc(views_doc);
    diagnostics.extend(d);
    let (dimensions, d) = config
        .doc("dimensions")
        .map(DerivedDimensions::from_doc)
        .unwrap_or_default();
    diagnostics.extend(d);
    let (sources, d) = config
        .doc("sources")
        .map(|doc| SourceSpec::from_doc(doc, &schema))
        .unwrap_or_default();
    diagnostics.extend(d);
    Some(DataSetup {
        config: DataServiceConfig {
            db_path,
            schema,
            views: views.clone(),
            dimensions: dimensions.clone(),
            query_workers: 4,
            sources,
        },
        views,
        dimensions,
        diagnostics,
    })
}

/// `[app] data.db_path` wins; then the demo directory; then the platform
/// application-data directory (spec §5.4).
pub fn db_path(
    config: &Config,
    demo: Option<&Path>,
    local_app_data: Option<String>,
    home: Option<String>,
) -> PathBuf {
    if let Some(p) = config.get("app", "data.db_path").and_then(|v| v.as_str()) {
        return PathBuf::from(p);
    }
    if let Some(dir) = demo {
        return dir.join("geode.duckdb");
    }
    if let Some(lad) = local_app_data {
        return PathBuf::from(lad).join("Geode").join("geode.duckdb");
    }
    let home = home.unwrap_or_else(|| ".".into());
    if cfg!(target_os = "macos") {
        PathBuf::from(home).join("Library/Application Support/Geode/geode.duckdb")
    } else {
        PathBuf::from(home).join(".local/share/geode/geode.duckdb")
    }
}

/// `[app] blotter.stale_after` (spec §6.5), a duration string in the
/// same `"30s"`/`"10m"`/`"2h"` vocabulary `sources.toml` already uses —
/// reused here (`geode_data::source::parse_duration`) rather than
/// inventing a second duration grammar. Missing or unparsable falls
/// back to the blotter's own default (`geode_blotter::tile::
/// DEFAULT_STALE_AFTER`, 15 minutes).
pub fn stale_after_from_config(config: &Config) -> Duration {
    config
        .get("app", "blotter.stale_after")
        .and_then(|v| v.as_str())
        .and_then(geode_data::source::parse_duration)
        .unwrap_or(geode_blotter::tile::DEFAULT_STALE_AFTER)
}

/// The sink `DataService::spawn` is given: `try_send` onto `tx`, never
/// blocking the caller (query-pool worker or ingest thread), and a
/// refusal bumps `dropped` rather than being lost silently (§7.3).
/// Factored out of `start` so it's unit-testable without a real
/// service thread.
fn make_sink(tx: async_channel::Sender<DataEvent>, dropped: Arc<AtomicU64>) -> EventSink {
    Arc::new(move |e| {
        if tx.try_send(e).is_ok() {
            true
        } else {
            dropped.fetch_add(1, Ordering::Relaxed);
            false
        }
    })
}

pub struct Bridge {
    pub handle: DataHandle,
    pub factory: Rc<BlotterFactory>,
    events: async_channel::Receiver<DataEvent>,
    dropped: Arc<AtomicU64>,
}

pub fn start(
    setup: DataSetup,
    find_style: FindStyle,
    stale_after: Duration,
    _cx: &mut App,
) -> Bridge {
    for d in &setup.diagnostics {
        tracing::warn!(target: "geode::query", "{d}");
    }
    let (tx, rx) = async_channel::bounded::<DataEvent>(EVENT_BOUND);
    let dropped = Arc::new(AtomicU64::new(0));
    let sink = make_sink(tx, dropped.clone());
    // Cloned before the move into `DataService::spawn` below — the
    // factory validates `:filter`/`:scope` against the same schema and
    // dimensions the service itself was built from (spec §3.7).
    let schema = setup.config.schema.clone();
    let dimensions = setup.dimensions.clone();
    let handle = DataService::spawn(setup.config, sink);
    let factory = Rc::new(BlotterFactory::new(
        handle.clone(),
        setup.views,
        schema,
        dimensions,
        find_style,
        stale_after,
    ));
    Bridge {
        handle,
        factory,
        events: rx,
        dropped,
    }
}

/// Route events into the shell and forward reloads. Wakes on arrival:
/// `async_channel::Receiver::recv` is a future gpui's executor polls, so
/// delivery latency is a frame, not a poll interval.
pub fn attach(bridge: &Bridge, window: WindowHandle<Root>, cx: &mut App) {
    let rx = bridge.events.clone();
    let dropped = bridge.dropped.clone();
    let handle = bridge.handle.clone();
    let factory = bridge.factory.clone();

    let shell = window
        .read(cx)
        .ok()
        .and_then(|root| root.view().clone().downcast::<ShellView>().ok())
        .expect("the window's root view is the shell");

    // Reloads: new views to the data thread and to the factory.
    cx.subscribe(&shell, {
        let handle = handle.clone();
        let factory = factory.clone();
        move |shell, event: &ShellEvent, cx| match event {
            ShellEvent::ConfigReloaded => {
                let config = shell.read(cx).config();
                let Some(views_doc) = config.doc("views") else {
                    return;
                };
                let (views, _) = ViewSpec::from_doc(views_doc);
                let (dims, _) = config
                    .doc("dimensions")
                    .map(DerivedDimensions::from_doc)
                    .unwrap_or_default();
                // `stale_after` is not among `ConfigReloaded`'s own
                // triggers (that event fires for `views`/`dimensions`
                // only — `shell::hot_reload::apply_reload`) — an edit to
                // `[app] blotter.stale_after` alone does not itself wake
                // this handler. It is re-read and re-applied here anyway,
                // piggybacking on whatever reload did fire, so it never
                // drifts further than one views/dimensions reload behind
                // what's on disk; a `stale_after`-only edit needs a
                // restart, same as `sources`/`datasets`.
                factory.set_views(views.clone());
                factory.set_find_style(FindStyle::from_config(config));
                factory.set_stale_after(stale_after_from_config(config));
                // `:filter`/`:scope` validation (Phase 4a §3.7): the
                // `datasets` doc is re-read here too — `ConfigReloaded`
                // doesn't fire for a `datasets`-only edit (that instead
                // sets `restart_required`, since the data engine itself
                // needs a restart to pick up new source paths or column
                // definitions), so this can only ever drift as far as a
                // `views`/`dimensions` reload that happened to arrive
                // alongside a stale `datasets` doc — the same bound
                // `stale_after` above accepts.
                if let Some(schema) = config.doc("datasets").map(|d| SchemaSpec::from_doc(d).0) {
                    factory.set_schema(schema);
                }
                factory.set_dims(dims.clone());
                handle.replace_views(views, dims);
            }
            // The dimension pickers (Phase 4a §3.3/§3.4): `geode-shell`
            // cannot depend on `geode-data` (CLAUDE.md), so a picker's
            // request leaves the shell as this event instead of a direct
            // `DataHandle::distinct` call — this is the one place that
            // call actually happens. The outcome comes back on the
            // `DataEvent` drain loop below, routed to `deliver_distinct`.
            // A refused request (`false`: the query pool's queue is full
            // or the service thread is gone) gets no reply from that
            // drain loop — nothing else would ever arrive to move the
            // picker off `loading…` — so a synthetic error outcome is
            // delivered right here instead, echoing the request's own
            // key/tag/column exactly as a real reply would.
            ShellEvent::DistinctRequested(params) => {
                let queued = handle.distinct(params.clone());
                if !queued {
                    let outcome = DistinctOutcome {
                        key: params.key,
                        tag: params.tag,
                        column: params.column.clone(),
                        values: Err("the data service is busy or gone — try again".into()),
                    };
                    shell.update(cx, |s, cx| s.deliver_distinct(outcome, cx));
                }
            }
            ShellEvent::RestartRequired(_) => {}
        }
    })
    .detach();

    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut last_dropped = 0u64;
        while let Ok(event) = rx.recv().await {
            let now_dropped = dropped.load(Ordering::Relaxed);
            // Every branch below reaches the shell through `window.update`
            // rather than a standalone `Entity<ShellView>` clone updated
            // via plain `cx.update`: an entity update succeeds forever,
            // window or no, so a bare `cx.update` never notices the
            // window is gone and this task (with its `DataHandle` clone,
            // `Rc<BlotterFactory>` and `Arc<AtomicU64>`) would outlive the
            // window until a `Query` outcome happened to arrive. Routing
            // every branch through the window handle makes the very next
            // event — of any kind — the one that ends the task.
            let handled = window.update(cx, |root, window, cx| {
                let Ok(shell) = root.view().clone().downcast::<ShellView>() else {
                    return;
                };
                if now_dropped != last_dropped {
                    shell.update(cx, |s, cx| {
                        s.set_data_status(Some(format!("data: {now_dropped} event(s) dropped")), cx)
                    });
                }
                match event {
                    DataEvent::Query(outcome) => {
                        shell.update(cx, |s, cx| s.deliver(outcome, window, cx));
                    }
                    DataEvent::Published {
                        dataset,
                        batch,
                        books,
                        ..
                    } => {
                        // Not logged here (Phase 4b Task 2 fix round 1,
                        // MAJ-2): `geode-data`'s own `service.rs` already
                        // logs every publish at `info` — with more detail
                        // (book/row counts) than this arm has — the
                        // moment the event is constructed, so a second
                        // line here would be a strictly less informative
                        // duplicate, and it would run on the UI thread
                        // (this whole match is inside `window.update`),
                        // against the Global Constraint that UI-thread
                        // code emits at `warn` or above only. Deleting
                        // beats dropping to `debug`: a `debug!` call site
                        // here would cost a (statically-disabled, but
                        // real) max-level check on every publish for no
                        // reason to exist at all — there's nothing this
                        // arm could say that the engine-side line
                        // doesn't already say better.
                        let frame = shell.read(cx).frame().clone();
                        // The event carries no timestamp of its own; the
                        // arrival instant is what a "recent publishes"
                        // preset needs (Phase 4a §3.12).
                        let publish = geode_shell::frame::Publish {
                            dataset,
                            batch,
                            books: books.len(),
                            at: chrono::Utc::now(),
                        };
                        frame.update(cx, |f, cx| {
                            f.note_published(publish);
                            cx.notify();
                        });
                    }
                    DataEvent::Health { source, worst, .. } => {
                        // Not logged here either, same reasoning as
                        // `Published` just above (MAJ-2): `geode-data`'s
                        // `log_health_event` already logs this at the
                        // level the outcome deserves (`error` for
                        // `Failed`, `warn` for `Degraded`/
                        // `PendingTooLong`, `info`/`debug` otherwise) the
                        // moment the event is constructed. This arm keeps
                        // only the real UI state change — the status bar
                        // — which is not logging and so isn't subject to
                        // the UI-thread level constraint at all.
                        shell.update(cx, |s, cx| {
                            s.set_data_status(Some(format!("{source}: {}", worst.label())), cx)
                        });
                    }
                    DataEvent::Diagnostics(diags) => {
                        for d in diags {
                            tracing::warn!(target: "geode::query", "{d}");
                        }
                    }
                    // The dimension picker's own outcome (spec §3.4),
                    // paired with the `DistinctRequested` submission
                    // above. `deliver_distinct` carries its own
                    // stale-tag/stale-column/no-picker-open guard, so
                    // nothing here needs to check the key or tag itself.
                    DataEvent::Distinct(outcome) => {
                        shell.update(cx, |s, cx| s.deliver_distinct(outcome, cx));
                    }
                }
            });
            if handled.is_err() {
                return; // the window is gone
            }
            last_dropped = now_dropped;
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{ConfigSources, LayerDoc};
    use geode_shell::actions::ActionRegistry;
    use geode_shell::defaults::{BUILTIN_KEYMAP, default_mod, register_builtin_actions};
    use geode_shell::keymap::build_keymap;
    use geode_shell::module::ModuleRoster;
    use geode_shell::session::TileRecords;
    use geode_shell::shell::ShellServices;
    use geode_shell::tiling::Workspaces;
    use geode_shell::{theme, vimfind::FindStyle};
    use gpui::AppContext as _;

    /// The minimal real `ShellServices` a window needs to open — same
    /// shape as `geode-shell`'s own `test_services()` (not reachable
    /// from here: it is `pub(super)` inside that crate's test module),
    /// built from public items only.
    fn test_shell_services() -> ShellServices {
        test_shell_services_with_config(Config::load(&ConfigSources::default()))
    }

    /// A trimmed `datasets` doc making `book` pickable (categorical
    /// dimension) — same shape as `geode-shell::shell::tests::picker`'s
    /// own `DATASETS_DOC`, reproduced here since that module is private
    /// to its crate.
    const PICKABLE_DATASETS_DOC: &str = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
"#;

    /// `test_shell_services()` with `book` pickable, for the picker test
    /// below — `picker::open` only reaches the `Values` stage (and so
    /// only emits `DistinctRequested`) for a column `ShellView.pickable`
    /// actually names.
    fn test_shell_services_with_pickable_book() -> ShellServices {
        test_shell_services_with_config(Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("datasets", PICKABLE_DATASETS_DOC).unwrap()],
            desk: None,
            user: None,
        }))
    }

    fn test_shell_services_with_config(config: Config) -> ShellServices {
        let mut registry = ActionRegistry::default();
        register_builtin_actions(&mut registry);
        let mod_alias = default_mod();
        let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
        let (keymap, diags) = build_keymap(&[doc], mod_alias, &registry);
        assert!(diags.is_empty(), "{diags:?}");
        let (theme, warnings) = theme::load_bundled();
        assert!(warnings.is_empty(), "{warnings:?}");
        ShellServices {
            config,
            registry,
            keymap,
            mod_alias,
            workspaces: Workspaces::new(),
            theme,
            session_path: None,
            roster: ModuleRoster::default(),
            restored_tiles: TileRecords::new(),
            restored_frame: None,
            log: None,
        }
    }

    fn open_test_window(
        cx: &mut gpui::TestAppContext,
        services: ShellServices,
    ) -> WindowHandle<Root> {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| ShellView::new(services, None, None, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
        })
        .unwrap()
    }

    /// Finding 1 (fix round 1): every branch of the drain loop must
    /// detect the closed window and end the task, not just `Query`.
    /// Before the fix, `Published`/`Health`/the dropped-events branch
    /// reached the shell through a standalone `Entity<ShellView>` clone
    /// via plain `cx.update`, which succeeds forever regardless of the
    /// window — so the task (and its `dropped` counter, `DataHandle` and
    /// factory clones) outlived the window until a `Query` outcome
    /// happened to arrive.
    ///
    /// `dropped: Arc<AtomicU64>` is the cleanest observable proxy for
    /// "the task has ended": it is captured directly by the drain task's
    /// `async move` block (every branch reads it) and by nothing else in
    /// `attach` — unlike `factory`/`handle`, which the `ConfigReloaded`
    /// subscription also clones for its own, unrelated, longer lifetime.
    /// A `Health` event — one of the branches that used to bypass the
    /// window check — is sent after the window closes; if the task is
    /// still alive it will have observed the event and still hold its
    /// clone, so the strong count would not move. No completion flag
    /// needed in production code.
    #[gpui::test]
    fn the_drain_task_ends_on_the_first_event_after_the_window_closes(
        cx: &mut gpui::TestAppContext,
    ) {
        let window = open_test_window(cx, test_shell_services());

        let (handle, _rx) = DataHandle::for_tests();
        let factory = Rc::new(BlotterFactory::new(
            handle.clone(),
            Vec::new(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::default(),
            Duration::from_secs(900),
        ));
        let (tx, rx) = async_channel::bounded::<DataEvent>(EVENT_BOUND);
        let dropped = Arc::new(AtomicU64::new(0));
        let bridge = Bridge {
            handle,
            factory,
            events: rx,
            dropped: dropped.clone(),
        };

        cx.update(|cx| attach(&bridge, window, cx));

        // This test's own `dropped`, `bridge.dropped`, and the drain
        // task's own clone (captured at `attach` time) — three, while
        // the task is alive.
        let alive = Arc::strong_count(&dropped);
        assert_eq!(
            alive, 3,
            "the drain task holds its own clone of `dropped` while it runs"
        );

        cx.update(|cx| {
            window
                .update(cx, |_, window, _| window.remove_window())
                .unwrap();
        });
        cx.run_until_parked();

        // A `Health` event — one of the branches that used to bypass the
        // window check entirely — must be what ends the task now that
        // the window is gone.
        tx.try_send(DataEvent::Health {
            source: "s".into(),
            worst: geode_data::health::Health::Ok,
            detail: String::new(),
        })
        .unwrap();
        cx.run_until_parked();

        assert_eq!(
            Arc::strong_count(&dropped),
            alive - 1,
            "the drain task released its clone of `dropped` once the window was gone"
        );
    }

    /// F3 (final fix wave, whole-branch review): `DataHandle::distinct`
    /// discarding its `bool` used to leave the picker on "loading…"
    /// forever once the request was refused, since nothing else would
    /// ever reply. `DataHandle::shutdown` drops the request sender, so
    /// every later `distinct` call refuses (`Inner::send` sees `None`)
    /// without needing a real service thread to exercise the refusal.
    #[gpui::test]
    fn a_refused_distinct_request_errors_the_picker(cx: &mut gpui::TestAppContext) {
        let window = open_test_window(cx, test_shell_services_with_pickable_book());
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let (handle, _rx) = DataHandle::for_tests();
        handle.shutdown();
        let factory = Rc::new(BlotterFactory::new(
            handle.clone(),
            Vec::new(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::default(),
            Duration::from_secs(900),
        ));
        let (_tx, rx) = async_channel::bounded::<DataEvent>(EVENT_BOUND);
        let bridge = Bridge {
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
        };
        cx.update(|cx| attach(&bridge, window, cx));

        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        vcx.update(|window, cx| {
            shell.update(cx, |view, cx| {
                geode_shell::shell::picker::open(view, Some("book".into()), window, cx);
            });
        });
        vcx.run_until_parked();

        let values = shell.read_with(&vcx, |s, _| {
            s.picker()
                .expect("the picker is still open — nothing here closes it")
                .values
                .clone()
        });
        assert_eq!(
            values,
            Some(Err(
                "the data service is busy or gone — try again".to_string()
            )),
            "a refused request must error the picker, not leave it loading forever"
        );
    }

    #[test]
    fn the_database_path_prefers_config_then_demo_then_the_platform_dir() {
        let empty = Config::load(&ConfigSources::default());
        assert_eq!(
            db_path(
                &empty,
                None,
                Some("C:\\Users\\me\\AppData\\Local".into()),
                Some("/home/me".into())
            ),
            PathBuf::from("C:\\Users\\me\\AppData\\Local")
                .join("Geode")
                .join("geode.duckdb")
        );
        let unix = db_path(&empty, None, None, Some("/home/me".into()));
        assert!(
            unix.ends_with("Geode/geode.duckdb") || unix.ends_with("geode/geode.duckdb"),
            "{unix:?}"
        );
        assert_eq!(
            db_path(
                &empty,
                Some(std::path::Path::new("/tmp/geode-demo/100000-42")),
                None,
                None
            ),
            PathBuf::from("/tmp/geode-demo/100000-42/geode.duckdb")
        );
        let configured = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin("app", "[data]\ndb_path = \"/var/geode/x.duckdb\"\n").unwrap(),
            ],
            ..ConfigSources::default()
        });
        assert_eq!(
            db_path(
                &configured,
                Some(std::path::Path::new("/tmp/d")),
                None,
                None
            ),
            PathBuf::from("/var/geode/x.duckdb"),
            "config wins even over demo"
        );
    }

    #[test]
    fn data_setup_needs_datasets_and_views_and_carries_sources() {
        let none = Config::load(&ConfigSources::default());
        assert!(data_setup(&none, "/tmp/x.duckdb".into()).is_none());
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "datasets",
                    "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
                )
                .unwrap(),
                LayerDoc::builtin("views", "[v]\ndataset = \"risk\"\ngrouping = [\"book\"]\n").unwrap(),
                LayerDoc::builtin("sources", "[s]\ndataset = \"risk\"\npaths = [\"/x/*.csv\"]\n").unwrap(),
            ],
            ..ConfigSources::default()
        });
        let setup = data_setup(&config, "/tmp/x.duckdb".into()).unwrap();
        assert_eq!(setup.config.sources.len(), 1);
        assert_eq!(setup.views.len(), 1);
        assert_eq!(setup.config.query_workers, 4);
    }

    #[test]
    fn stale_after_reads_the_configured_duration_and_falls_back_to_the_default() {
        let none = Config::load(&ConfigSources::default());
        assert_eq!(
            stale_after_from_config(&none),
            geode_blotter::tile::DEFAULT_STALE_AFTER
        );
        let configured = Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", "[blotter]\nstale_after = \"90s\"\n").unwrap()],
            ..ConfigSources::default()
        });
        assert_eq!(
            stale_after_from_config(&configured),
            Duration::from_secs(90)
        );
        let junk = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin("app", "[blotter]\nstale_after = \"not-a-duration\"\n").unwrap(),
            ],
            ..ConfigSources::default()
        });
        assert_eq!(
            stale_after_from_config(&junk),
            geode_blotter::tile::DEFAULT_STALE_AFTER,
            "an unparsable value falls back rather than panicking"
        );
    }

    #[test]
    fn a_refused_event_is_counted_as_dropped_rather_than_lost_silently() {
        let (tx, rx) = async_channel::bounded::<DataEvent>(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let sink = make_sink(tx, dropped.clone());
        assert!(sink(DataEvent::Diagnostics(Vec::new())), "the first fits");
        assert!(
            !sink(DataEvent::Diagnostics(Vec::new())),
            "the channel is now full"
        );
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        drop(rx);
    }
}
