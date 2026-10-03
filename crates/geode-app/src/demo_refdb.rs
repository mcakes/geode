//! Demo mode's reference database: the ten demo underlyings with
//! hand-written vendor tickers, currencies, calendars and exchanges. Every
//! third poll revises one name, so generations and time travel have
//! something to show. `fail_next` and `delay` exercise the degraded and slow
//! paths. Implements only the snapshot side.

use geode_core::reference::{RefColumn, TableRows};
use geode_data::adapter::{Adapter, AdapterError, Egress, SnapshotQuery, Subscription};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The demo reference database's adapter name, as the `[refdb]` source
/// names it.
pub const DEMO_REFDB: &str = "demo_refdb";
const TABLE: &str = "underlyings";

/// `(underlying_ref, name, bbg_ticker, ric, currency, calendar, exchange,
/// multiplier)`.
type Row = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    f64,
);

const ROWS: [Row; 10] = [
    (
        "SPX",
        "S&P 500",
        "SPX Index",
        ".SPX",
        "USD",
        "XNYS",
        "XCBO",
        100.0,
    ),
    (
        "SX5E",
        "EURO STOXX 50",
        "SX5E Index",
        ".STOXX50E",
        "EUR",
        "XEUR",
        "XEUR",
        10.0,
    ),
    (
        "NKY",
        "Nikkei 225",
        "NKY Index",
        ".N225",
        "JPY",
        "XTKS",
        "XOSE",
        1000.0,
    ),
    (
        "UKX",
        "FTSE 100",
        "UKX Index",
        ".FTSE",
        "GBP",
        "XLON",
        "IFLL",
        10.0,
    ),
    (
        "NDX",
        "Nasdaq-100",
        "NDX Index",
        ".NDX",
        "USD",
        "XNAS",
        "XCBO",
        100.0,
    ),
    (
        "RTY",
        "Russell 2000",
        "RTY Index",
        ".RUT",
        "USD",
        "XNYS",
        "XCBO",
        100.0,
    ),
    (
        "DAX",
        "DAX",
        "DAX Index",
        ".GDAXI",
        "EUR",
        "XETR",
        "XEUR",
        5.0,
    ),
    (
        "SMI",
        "Swiss Market Index",
        "SMI Index",
        ".SSMI",
        "CHF",
        "XSWX",
        "XEUR",
        10.0,
    ),
    (
        "HSI",
        "Hang Seng",
        "HSI Index",
        ".HSI",
        "HKD",
        "XHKG",
        "XHKF",
        50.0,
    ),
    (
        "KOSPI2",
        "KOSPI 200",
        "KOSPI2 Index",
        ".KS200",
        "KRW",
        "XKRX",
        "XKRX",
        250_000.0,
    ),
];

/// The table at the given poll count. Revision `poll / 3` renames row
/// `(revision - 1) % 10`, so the content changes exactly when the revision
/// does and the service's unchanged check skips the two polls between.
pub fn table(poll: u64) -> TableRows {
    let revision = poll / 3;
    let renamed = (revision > 0).then(|| ((revision - 1) % ROWS.len() as u64) as usize);
    let text = |f: fn(&Row) -> &str| {
        RefColumn::Utf8(ROWS.iter().map(|r| Some(f(r).to_string())).collect())
    };
    let names = RefColumn::Utf8(
        ROWS.iter()
            .enumerate()
            .map(|(i, r)| {
                Some(if renamed == Some(i) {
                    format!("{} · rev {revision}", r.1)
                } else {
                    r.1.to_string()
                })
            })
            .collect(),
    );
    TableRows {
        columns: vec![
            ("underlying_ref".into(), text(|r| r.0)),
            ("name".into(), names),
            ("bbg_ticker".into(), text(|r| r.2)),
            ("ric".into(), text(|r| r.3)),
            ("currency".into(), text(|r| r.4)),
            ("calendar".into(), text(|r| r.5)),
            ("exchange".into(), text(|r| r.6)),
            (
                "asset_type".into(),
                RefColumn::Utf8(vec![Some("index".into()); ROWS.len()]),
            ),
            (
                "multiplier".into(),
                RefColumn::F64(ROWS.iter().map(|r| Some(r.7)).collect()),
            ),
        ],
    }
}

/// The demo reference database. Every query handle shares one poll counter
/// and one pending failure, so `fail_next` reaches whichever handle the
/// snapshot worker holds.
pub struct DemoRefDb {
    polls: Arc<AtomicU64>,
    fail_next: Arc<Mutex<Option<String>>>,
    delay: Duration,
}

impl DemoRefDb {
    /// Each query sleeps `delay` before answering.
    pub fn new(delay: Duration) -> Arc<DemoRefDb> {
        Arc::new(DemoRefDb {
            polls: Arc::new(AtomicU64::new(0)),
            fail_next: Arc::new(Mutex::new(None)),
            delay,
        })
    }

    /// The next query, from any handle, fails with `message`. Only tests
    /// drive the degraded path today; the demo app never fails on its own.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn fail_next(&self, message: &str) {
        *self.fail_next.lock().unwrap_or_else(|e| e.into_inner()) = Some(message.to_string());
    }
}

struct DemoQuery {
    polls: Arc<AtomicU64>,
    fail_next: Arc<Mutex<Option<String>>>,
    delay: Duration,
}

impl SnapshotQuery for DemoQuery {
    fn query(&mut self, table_name: &str) -> Result<TableRows, AdapterError> {
        if table_name != TABLE {
            return Err(AdapterError {
                message: format!("unknown table '{table_name}'"),
            });
        }
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        // A failed query does not count as a poll: the next success answers
        // the revision the failure would have.
        if let Some(message) = self
            .fail_next
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            return Err(AdapterError { message });
        }
        Ok(table(self.polls.fetch_add(1, Ordering::Relaxed)))
    }
}

impl Adapter for DemoRefDb {
    fn name(&self) -> &'static str {
        DEMO_REFDB
    }
    fn subscription(&self) -> Option<Box<dyn Subscription>> {
        None
    }
    fn egress(&self) -> Option<Box<dyn Egress>> {
        None
    }
    fn snapshot(&self) -> Option<Box<dyn SnapshotQuery>> {
        Some(Box::new(DemoQuery {
            polls: self.polls.clone(),
            fail_next: self.fail_next.clone(),
            delay: self.delay,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name_of(t: &TableRows, i: usize) -> String {
        match &t.columns.iter().find(|(n, _)| n == "name").unwrap().1 {
            RefColumn::Utf8(v) => v[i].clone().unwrap(),
            _ => unreachable!(),
        }
    }

    #[test]
    fn every_demo_underlying_has_a_row() {
        let t = table(0);
        let refs = match &t.columns[0].1 {
            RefColumn::Utf8(v) => v.clone(),
            _ => unreachable!(),
        };
        let refs: Vec<String> = refs.into_iter().map(Option::unwrap).collect();
        assert_eq!(refs, geode_demo_data::demo_underlyings());
    }

    #[test]
    fn the_table_changes_only_every_third_poll() {
        assert_eq!(table(0), table(1));
        assert_eq!(table(1), table(2));
        assert_ne!(table(2), table(3));
        assert_eq!(table(3), table(5));
        assert!(name_of(&table(3), 0).ends_with("· rev 1"));
        // Revision 11 wraps back to the first row.
        assert!(name_of(&table(33), 0).ends_with("· rev 11"));
        assert_eq!(name_of(&table(33), 1), "EURO STOXX 50");
    }

    /// The demo table conforms to the `underlyings` declaration the demo
    /// layer ships, so the `refdb` source's first poll publishes.
    #[test]
    fn the_demo_table_conforms_to_the_demo_dataset() {
        let config = geode_core::config::Config::load(&geode_core::config::ConfigSources {
            builtin: crate::demo::layer(std::path::Path::new("/tmp/x")),
            ..geode_core::config::ConfigSources::default()
        });
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        let (schema, d) = geode_core::schema::SchemaSpec::from_doc(config.doc("datasets").unwrap());
        assert!(d.is_empty(), "{d:?}");
        let ds = schema.dataset("underlyings").unwrap();
        let conformed = table(7).conform(ds).unwrap();
        assert_eq!(conformed.rows, 10);
        assert!(conformed.extra.is_empty(), "{:?}", conformed.extra);
        assert!(conformed.missing.is_empty(), "{:?}", conformed.missing);
    }

    #[test]
    fn fail_next_fails_exactly_one_query() {
        let db = DemoRefDb::new(Duration::ZERO);
        let mut q = db.snapshot().unwrap();
        q.query("underlyings").unwrap();
        q.query("underlyings").unwrap();
        db.fail_next("db down");
        assert_eq!(q.query("underlyings").unwrap_err().message, "db down");
        // The failure is not a poll: the next success is the third poll, still
        // revision 0, not the fourth's first revision.
        assert_eq!(q.query("underlyings").unwrap(), table(2));
    }

    #[test]
    fn an_unknown_table_is_an_error() {
        let db = DemoRefDb::new(Duration::ZERO);
        assert!(
            db.snapshot()
                .unwrap()
                .query("nope")
                .unwrap_err()
                .message
                .contains("nope")
        );
    }
}
