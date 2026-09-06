//! Where shell and data meet (Phase 3 spec §5.1, §9.1): builds the
//! service config from the layered docs, spawns the service behind a
//! `DataHandle`, drains its event channel into the shell on a task that
//! wakes on arrival, and forwards config reloads back to the data thread.

use geode_blotter::BlotterFactory;
use geode_core::config::{Config, Diagnostic};
use geode_core::dimensions::DerivedDimensions;
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
    /// field for the same reason. Nothing in this crate reads it back out
    /// today (`start`/`attach` only need `views`), so it's honestly
    /// unread here — not a live behaviour switch, same as `geode_shell::
    /// shell::ShellView`'s own `find_style` field doc describes for a
    /// comparable case.
    #[allow(dead_code)]
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
        eprintln!("[data] {d}");
    }
    let (tx, rx) = async_channel::bounded::<DataEvent>(EVENT_BOUND);
    let dropped = Arc::new(AtomicU64::new(0));
    let sink = make_sink(tx, dropped.clone());
    let handle = DataService::spawn(setup.config, sink);
    let factory = Rc::new(BlotterFactory::new(
        handle.clone(),
        setup.views,
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
        move |shell, event: &ShellEvent, cx| {
            if let ShellEvent::ConfigReloaded = event {
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
                handle.replace_views(views, dims);
            }
        }
    })
    .detach();

    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut last_dropped = 0u64;
        while let Ok(event) = rx.recv().await {
            let now_dropped = dropped.load(Ordering::Relaxed);
            if now_dropped != last_dropped {
                last_dropped = now_dropped;
                cx.update(|cx| {
                    shell.update(cx, |s, cx| {
                        s.set_data_status(Some(format!("data: {now_dropped} event(s) dropped")), cx)
                    });
                });
            }
            let outcome = match event {
                DataEvent::Query(outcome) => Some(outcome),
                DataEvent::Published {
                    dataset,
                    batch,
                    gen_id,
                    ..
                } => {
                    eprintln!("[data] published {dataset}/{batch} gen {gen_id}");
                    cx.update(|cx| {
                        let frame = shell.read(cx).frame().clone();
                        frame.update(cx, |f, cx| {
                            f.note_published();
                            cx.notify();
                        });
                    });
                    None
                }
                DataEvent::Health {
                    source,
                    worst,
                    detail,
                } => {
                    eprintln!("[data] health {source}: {} — {detail}", worst.label());
                    cx.update(|cx| {
                        shell.update(cx, |s, cx| {
                            s.set_data_status(Some(format!("{source}: {}", worst.label())), cx)
                        });
                    });
                    None
                }
                DataEvent::Diagnostics(diags) => {
                    for d in diags {
                        eprintln!("[data] {d}");
                    }
                    None
                }
            };
            if let Some(outcome) = outcome {
                let delivered = window.update(cx, |root, window, cx| {
                    if let Ok(shell) = root.view().clone().downcast::<ShellView>() {
                        shell.update(cx, |s, cx| s.deliver(outcome, window, cx));
                    }
                });
                if delivered.is_err() {
                    return; // the window is gone
                }
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{ConfigSources, LayerDoc};

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
