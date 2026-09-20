use crate::ingest::{LoadRequest, load_file};
use crate::source::Sentinel;
use crate::store::{Catalog, Store};
use geode_core::config::{LayerDoc, merge_docs};
use geode_core::schema::SchemaSpec;

#[test]
fn failed_second_grain_rolls_back_the_whole_file() {
    let dir = tempfile::tempdir().unwrap();
    let schema = r#"
[risk.columns.book]
type = "utf8"
role = "dimension"
[risk.columns.lhu]
type = "utf8"
role = "dimension"
[risk.columns.position_ref]
type = "utf8"
role = "key"
[risk.columns.counterparty]
type = "utf8"
role = "dimension"
[risk.columns.instrument_ref]
type = "utf8"
role = "key"
[risk.columns.pnl]
type = "f64"
role = "measure"
grain = "position"
[risk.columns.npv]
type = "f64"
role = "measure"
grain = "instrument"
"#;
    let doc = merge_docs(
        "datasets",
        &[LayerDoc::builtin("datasets", schema).unwrap()],
    );
    let (schema, diagnostics) = SchemaSpec::from_doc(&doc);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let ds = &schema.datasets[0];
    let store = Store::open(dir.path().join("probe.duckdb")).unwrap();
    store.apply_schema(ds).unwrap();
    Catalog::new(store.writer()).ensure_tables().unwrap();
    let csv = dir.path().join("risk.csv");
    let header = "book,lhu,position_ref,counterparty,instrument_ref,pnl,npv";
    std::fs::write(&csv, format!("{header}\nB,L,P,C,I,10,100\n")).unwrap();
    let mut sentinel = Sentinel {
        as_of: "2026-09-01T00:00:00Z".parse().unwrap(),
        columns: header.split(',').map(str::to_owned).collect(),
        books: vec!["B".into()],
        row_count: Some(1),
        dataset: None,
        business_date: None,
    };
    load_file(
        &store,
        &LoadRequest {
            dataset: ds,
            dataset_name: "risk",
            csv_path: &csv,
            sentinel: &sentinel,
            batch: "risk",
        },
    )
    .unwrap();
    // Fail the second grain after the first has been replaced inside the transaction.
    store
        .writer()
        .execute_batch("alter table risk_instrument_live rename to retained_instrument_live")
        .unwrap();
    std::fs::write(&csv, format!("{header}\nB,L,P,C,I,20,200\n")).unwrap();
    sentinel.as_of = "2026-09-02T00:00:00Z".parse().unwrap();
    let result = load_file(
        &store,
        &LoadRequest {
            dataset: ds,
            dataset_name: "risk",
            csv_path: &csv,
            sentinel: &sentinel,
            batch: "risk",
        },
    );
    assert!(result.is_err());
    let pnl: f64 = store
        .writer()
        .query_row("select pnl from risk_position_live", [], |r| r.get(0))
        .unwrap();
    let npv: f64 = store
        .writer()
        .query_row("select npv from retained_instrument_live", [], |r| r.get(0))
        .unwrap();

    assert_eq!((pnl, npv), (10., 100.));
    let generations: i64 = store
        .writer()
        .query_row("select count(*) from generations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(generations, 1);
    store
        .writer()
        .execute_batch("alter table retained_instrument_live rename to risk_instrument_live")
        .unwrap();
    load_file(
        &store,
        &LoadRequest {
            dataset: ds,
            dataset_name: "risk",
            csv_path: &csv,
            sentinel: &sentinel,
            batch: "risk",
        },
    )
    .unwrap();
    let pnl: f64 = store
        .writer()
        .query_row("select pnl from risk_position_live", [], |r| r.get(0))
        .unwrap();
    assert_eq!(pnl, 20.);
}

#[test]
fn fetch_panic_delivers_a_failure_outcome() {
    use crate::adapter::{AdapterError, Fetch, FetchRequest, SeriesRows};
    use crate::ingest::fetch::{FetchWork, FetchWorker};
    use std::sync::Arc;
    struct PanickingFetch;
    impl Fetch for PanickingFetch {
        fn fetch(&mut self, _: &FetchRequest) -> Result<SeriesRows, AdapterError> {
            panic!("injected vendor panic")
        }
        fn catalogue(&mut self) -> Option<Vec<String>> {
            None
        }
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let mut worker = FetchWorker::spawn(
        "probe",
        Box::new(PanickingFetch),
        Arc::new(move |outcome| {
            tx.send(outcome).unwrap();
        }),
    )
    .unwrap();
    assert!(worker.request(FetchWork::Span {
        identity: "SPX".into(),
        from: "2026-09-01T00:00:00Z".parse().unwrap(),
        to: "2026-09-02T00:00:00Z".parse().unwrap(),
    }));
    worker.shutdown();
    let outcome = rx.try_recv().expect("accepted fetch must complete");
    assert!(
        matches!(outcome, crate::ingest::fetch::FetchOutcome::Failed { identity, reason } if identity == "SPX" && reason.contains("injected vendor panic"))
    );
}
