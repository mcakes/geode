//! Resolve historical generations per partition and select them from live
//! and archive tables. Datasets and books refresh independently, so each
//! partition contributes its state at the requested instant. Live queries
//! read live tables directly and carry no generation predicate.

use crate::store::StoreError;
use chrono::{DateTime, Utc};
use duckdb::Connection;

/// Shared with the shell through `geode-core`, keeping the shell independent
/// of this data crate.
pub use geode_core::query::AsOf;

/// One partition's state at a point in time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedGeneration {
    pub batch: String,
    /// The bookless partition is represented by `None`. Ingest retains and
    /// reports these rows, so historical queries must match NULL books too.
    pub book: Option<String>,
    pub gen_id: i64,
    /// Source timestamp of the selected generation. Historical freshness uses
    /// this value, which can precede the requested instant.
    pub source_time: DateTime<Utc>,
}

/// Resolve the newest generation at or before `at` for each partition in the
/// dataset. Source-time ties select the greatest generation ID, matching the
/// retention ordering and choosing the corrected republish.
///
/// Read the `generations` summary to avoid scanning payload history during
/// query compilation. Publication and retention maintain it transactionally;
/// `store::ddl::rebuild_generations` reconstructs it from stored rows.
///
/// The summary must cover every grain's live and archive tables. A partition
/// can exist at only one grain, and its newest generation lives only in the
/// live table. Omitting either would silently narrow historical results.
pub fn resolve_generations(
    conn: &Connection,
    dataset: &str,
    at: DateTime<Utc>,
) -> Result<Vec<ResolvedGeneration>, StoreError> {
    let sql = "select batch, book, gen_id, source_time from (
             select batch, book, gen_id, source_time,
                    row_number() over (
                        partition by batch, book
                        order by source_time desc, gen_id desc
                    ) as rn
             from generations where dataset = ? and source_time <= ?
         ) where rn = 1";
    let err = |source| StoreError::Sql {
        statement: sql.to_string(),
        source,
    };
    let mut stmt = conn.prepare(sql).map_err(err)?;
    let rows = stmt
        .query_map(duckdb::params![dataset, at], |r| {
            Ok(ResolvedGeneration {
                batch: r.get(0)?,
                book: r.get(1)?,
                gen_id: r.get(2)?,
                source_time: r.get(3)?,
            })
        })
        .map_err(err)?;
    // Propagated, not swallowed. Discarding a row here narrows the
    // resolved generation set, which silently narrows the *result* — a
    // query that answers with less data than it should and says nothing.
    rows.collect::<Result<Vec<_>, _>>().map_err(err)
}

/// Test oracle that resolves generations by scanning payload tables. Compare
/// this independent path with summary-based resolution to check table coverage,
/// source-time ties, and summary maintenance.
#[cfg(test)]
pub(crate) fn resolve_from_tables(
    conn: &Connection,
    tables: &[String],
    at: DateTime<Utc>,
) -> Result<Vec<ResolvedGeneration>, StoreError> {
    if tables.is_empty() {
        return Ok(Vec::new());
    }
    let union = tables
        .iter()
        .map(|t| format!("select distinct batch, book, gen_id, source_time from {t}"))
        .collect::<Vec<_>>()
        .join(" union all ");
    let sql = format!(
        "select batch, book, gen_id, source_time from (
             select batch, book, gen_id, source_time,
                    row_number() over (
                        partition by batch, book
                        order by source_time desc, gen_id desc
                    ) as rn
             from ({union}) where source_time <= ?
         ) where rn = 1"
    );
    let err = |source| StoreError::Sql {
        statement: sql.clone(),
        source,
    };
    let mut stmt = conn.prepare(&sql).map_err(err)?;
    let rows = stmt
        .query_map(duckdb::params![at], |r| {
            Ok(ResolvedGeneration {
                batch: r.get(0)?,
                book: r.get(1)?,
                gen_id: r.get(2)?,
                source_time: r.get(3)?,
            })
        })
        .map_err(err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(err)
}

/// Select resolved generations with an ID prefilter and an exact tuple match
/// on `(batch, book, gen_id, source_time)`. Catalog strings are escaped as SQL
/// literals; user-supplied scope values use bound parameters elsewhere.
///
/// The source-time term remains necessary for stored history containing reused
/// generation IDs. Sequence allocation prevents new reuse after failed loads,
/// but cannot disambiguate rows already written with the same ID.
///
/// The ID list allows DuckDB scan filtering and row-group pruning even when
/// independently refreshed books have widely separated generation IDs. The
/// tuple semi-join enforces exact partition identity without a per-row chain
/// of alternatives. Its struct comparison matches NULL fields, so a typed
/// `NULL::varchar` represents a bookless partition without a sentinel string.
pub fn generation_predicate(generations: &[ResolvedGeneration]) -> String {
    if generations.is_empty() {
        // Selecting nothing, not everything: a time before all history is
        // an empty result, never the whole archive.
        return "false".to_string();
    }
    let mut ids: Vec<i64> = generations.iter().map(|g| g.gen_id).collect();
    ids.sort_unstable();
    ids.dedup();
    let id_list = ids
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let rows = generations
        .iter()
        .map(|g| {
            // A NULL book is `NULL::varchar`, typed so the column has a
            // type even when every generation's book is NULL — never a
            // sentinel string, which `book = '…'` could not match anyway.
            let book = match &g.book {
                Some(b) => format!("'{}'", b.replace('\'', "''")),
                None => "NULL::varchar".to_string(),
            };
            format!(
                "('{}', {book}, {}::bigint, '{}'::timestamptz)",
                g.batch.replace('\'', "''"),
                g.gen_id,
                g.source_time.to_rfc3339(),
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "(gen_id in ({id_list}) and \
         (batch, book, gen_id, source_time) in (select (b, k, g, t) from (values {rows}) v(b, k, g, t)))"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// Raw SQL fixtures bypass publication's summary maintenance. Rebuild the
    /// summary from fixture tables before resolving it by dataset name.
    fn resolve_after_rebuild(
        store: &crate::store::Store,
        dataset: &str,
        tables: &[String],
        at: DateTime<Utc>,
    ) -> Result<Vec<ResolvedGeneration>, StoreError> {
        crate::store::ddl::rebuild_generations(store.writer(), dataset, tables)?;
        resolve_generations(store.writer(), dataset, at)
    }

    /// An archive with two partitions refreshing on different clocks:
    /// BK000 at 07:00 and 14:00, BK001 only at 09:00.
    fn fixture() -> (tempfile::TempDir, crate::store::Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        crate::store::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        store
            .writer()
            .execute_batch(
                "create table risk_snapshot_position_archive(
                     book varchar, lhu varchar, position_ref varchar,
                     counterparty varchar, daily_trading_pnl double,
                     batch varchar, source_file_id bigint,
                     gen_id bigint, source_time timestamp with time zone);
                 insert into risk_snapshot_position_archive values
                   ('BK000','L','P1','C', 1, 'BK000', 1, 1, '2026-08-30T07:00:00Z'),
                   ('BK000','L','P1','C', 2, 'BK000', 2, 2, '2026-08-30T14:00:00Z'),
                   ('BK001','L','P2','C', 3, 'BK001', 3, 3, '2026-08-30T09:00:00Z');",
            )
            .unwrap();
        (dir, store)
    }

    #[test]
    fn resolves_the_newest_generation_at_or_before_the_request() {
        let (_d, store) = fixture();
        let gens = resolve_after_rebuild(
            &store,
            "ds",
            &["risk_snapshot_position_archive".to_string()],
            ts("2026-08-30T10:00:00Z"),
        )
        .unwrap();
        let book = |name: &str| {
            gens.iter()
                .find(|g| g.book.as_deref() == Some(name))
                .unwrap()
        };
        assert_eq!(book("BK000").gen_id, 1, "07:00, not the 14:00 generation");
        assert_eq!(book("BK001").gen_id, 3);
    }

    #[test]
    fn each_partition_resolves_on_its_own_clock() {
        // Books refresh independently, so historical resolution is per partition.
        let (_d, store) = fixture();
        let gens = resolve_after_rebuild(
            &store,
            "ds",
            &["risk_snapshot_position_archive".to_string()],
            ts("2026-08-30T08:00:00Z"),
        )
        .unwrap();
        assert_eq!(gens.len(), 1, "BK001 did not exist yet at 08:00: {gens:?}");
        assert_eq!(gens[0].book.as_deref(), Some("BK000"));
    }

    #[test]
    fn a_row_that_cannot_be_rebuilt_is_an_error_not_a_smaller_answer() {
        // An unreadable payload row must fail summary rebuilding instead of
        // silently narrowing the resolved history. Here the cast fails while
        // inserting into `generations`; the next test covers unreadable rows
        // already present in that summary.
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        crate::store::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        store
            .writer()
            .execute_batch(
                "create table bad_archive(
                     batch varchar, book varchar,
                     gen_id varchar, source_time timestamp with time zone);
                 insert into bad_archive values
                   ('b', 'BK000', 'not-a-number', '2026-08-30T07:00:00Z');",
            )
            .unwrap();
        let err = resolve_after_rebuild(
            &store,
            "ds",
            &["bad_archive".to_string()],
            ts("2026-08-30T10:00:00Z"),
        );
        assert!(err.is_err(), "an unreadable generation row must propagate");
    }

    #[test]
    fn a_summary_row_that_cannot_be_read_is_an_error_not_a_smaller_answer() {
        // A NULL batch is permitted by the summary table but cannot decode
        // into `ResolvedGeneration::batch`. Propagate that error rather than
        // dropping the row and silently narrowing the historical answer.
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        crate::store::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        store
            .writer()
            .execute_batch(
                "insert into generations values ('ds', NULL, 'BK000', 1, '2026-08-30T07:00:00Z');",
            )
            .unwrap();
        let err = resolve_generations(store.writer(), "ds", ts("2026-08-30T10:00:00Z"));
        assert!(err.is_err(), "an unreadable summary row must propagate");
    }

    #[test]
    fn a_tie_that_straddles_archive_and_live_resolves_to_the_live_one() {
        // A corrected republish has the same source time as its predecessor,
        // with the predecessor in archive and the correction in live. Resolve
        // the tie across both relations by greatest generation ID; otherwise
        // a historical query can return the superseded numbers.
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        crate::store::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        store
            .writer()
            .execute_batch(
                "create table straddle_archive(
                     book varchar, batch varchar, gen_id bigint,
                     source_time timestamp with time zone);
                 create table straddle_live(
                     book varchar, batch varchar, gen_id bigint,
                     source_time timestamp with time zone);
                 insert into straddle_archive values
                   ('BK000', 'b', 5, '2026-08-30T07:00:00Z');
                 insert into straddle_live values
                   ('BK000', 'b', 6, '2026-08-30T07:00:00Z');",
            )
            .unwrap();

        let gens = resolve_after_rebuild(
            &store,
            "ds",
            &["straddle_archive".to_string(), "straddle_live".to_string()],
            ts("2026-08-30T10:00:00Z"),
        )
        .unwrap();
        assert_eq!(gens.len(), 1, "one generation per partition");
        assert_eq!(
            gens[0].gen_id, 6,
            "the correction wins across the archive/live union, not the \
             copy it replaced"
        );
    }

    #[test]
    fn a_tie_on_source_time_resolves_to_the_newest_gen_id_every_time() {
        // Same-time corrections require the greatest generation ID to win.
        // Insert eight tied generations in ascending ID order so taking the
        // first row disagrees with that rule. Repeating a deterministic query
        // is not independent sampling; the fixture must expose the tie-break.
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        crate::store::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        store
            .writer()
            .execute_batch(
                "create table risk_snapshot_position_archive(
                     book varchar, batch varchar, gen_id bigint,
                     source_time timestamp with time zone);
                 insert into risk_snapshot_position_archive values
                   ('BK000', 'b', 1, '2026-08-30T07:00:00Z'),
                   ('BK000', 'b', 2, '2026-08-30T07:00:00Z'),
                   ('BK000', 'b', 3, '2026-08-30T07:00:00Z'),
                   ('BK000', 'b', 4, '2026-08-30T07:00:00Z'),
                   ('BK000', 'b', 5, '2026-08-30T07:00:00Z'),
                   ('BK000', 'b', 6, '2026-08-30T07:00:00Z'),
                   ('BK000', 'b', 7, '2026-08-30T07:00:00Z'),
                   ('BK000', 'b', 8, '2026-08-30T07:00:00Z');",
            )
            .unwrap();
        crate::store::ddl::rebuild_generations(
            store.writer(),
            "ds",
            &["risk_snapshot_position_archive".to_string()],
        )
        .unwrap();
        for _ in 0..20 {
            let gens =
                resolve_generations(store.writer(), "ds", ts("2026-08-30T10:00:00Z")).unwrap();
            assert_eq!(gens.len(), 1);
            assert_eq!(gens[0].gen_id, 8, "the correction wins, deterministically");
        }
    }

    #[test]
    fn the_predicate_names_the_source_time_so_a_reused_gen_id_selects_one_generation() {
        // Legacy history can contain reused generation IDs after failed loads.
        // For the same partition and ID, source time must distinguish the rows.
        // Current publications allocate IDs from a sequence.
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        crate::store::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        store
            .writer()
            .execute_batch(
                "create table risk_snapshot_position_archive(
                     book varchar, batch varchar, gen_id bigint, pnl double,
                     source_time timestamp with time zone);
                 insert into risk_snapshot_position_archive values
                   ('BK000', 'b', 1, 100, '2026-08-30T07:00:00Z'),
                   ('BK000', 'b', 1, 5, '2026-08-30T09:00:00Z');",
            )
            .unwrap();
        let gens = resolve_after_rebuild(
            &store,
            "ds",
            &["risk_snapshot_position_archive".to_string()],
            ts("2026-08-30T10:00:00Z"),
        )
        .unwrap();
        let total: f64 = store
            .writer()
            .query_row(
                &format!(
                    "select sum(pnl) from risk_snapshot_position_archive where {}",
                    generation_predicate(&gens)
                ),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(total, 5.0, "the 09:00 generation alone, not both");
    }

    #[test]
    fn a_time_before_all_history_resolves_to_nothing() {
        let (_d, store) = fixture();
        let gens = resolve_after_rebuild(
            &store,
            "ds",
            &["risk_snapshot_position_archive".to_string()],
            ts("2026-08-29T00:00:00Z"),
        )
        .unwrap();
        assert!(gens.is_empty());
    }

    #[test]
    fn the_predicate_selects_exactly_the_resolved_generations() {
        let (_d, store) = fixture();
        let gens = resolve_after_rebuild(
            &store,
            "ds",
            &["risk_snapshot_position_archive".to_string()],
            ts("2026-08-30T10:00:00Z"),
        )
        .unwrap();
        let pred = generation_predicate(&gens);
        let total: f64 = store
            .writer()
            .query_row(
                &format!(
                    "select sum(daily_trading_pnl) from risk_snapshot_position_archive \
                     where {pred}"
                ),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(total, 4.0, "BK000's 07:00 row plus BK001's, not the 14:00");
    }

    #[test]
    fn the_predicate_names_a_gen_id_in_list_and_no_or_chain() {
        // The predicate lists the resolved generation IDs with IN.
        // Books refresh independently, so a `[lo, hi]` range can span
        // most of the archive and defeat pruning. See docs/perf.md,
        // "the range prefilter degenerates on a real archive".
        let (_d, store) = fixture();
        let gens = resolve_after_rebuild(
            &store,
            "ds",
            &["risk_snapshot_position_archive".to_string()],
            ts("2026-08-30T10:00:00Z"),
        )
        .unwrap();
        let pred = generation_predicate(&gens);
        assert!(
            pred.contains("gen_id in (1, 3)"),
            "the IN-list names exactly the resolved gen_ids (1 and 3): {pred}"
        );
        assert!(!pred.contains(" or "), "no per-generation OR chain: {pred}");
        assert!(!pred.contains("between"), "no range term: {pred}");
    }

    #[test]
    fn the_predicate_selects_a_null_book_partition_from_either_side() {
        // Give archive and live rows the same generation ID and NULL book,
        // but different source times. The historical predicate must select
        // only the earlier archived value. The ID prefilter cannot separate
        // these rows: this fixture requires both the source-time tuple field
        // and NULL-safe struct matching to do real work.
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        crate::store::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        store
            .writer()
            .execute_batch(
                "create table bookless_position_archive(
                     batch varchar, book varchar, gen_id bigint, pnl double,
                     source_time timestamp with time zone);
                 create table bookless_position_live(
                     batch varchar, book varchar, gen_id bigint, pnl double,
                     source_time timestamp with time zone);
                 insert into bookless_position_archive values
                   ('b', NULL, 1, 100, '2026-08-30T07:00:00Z');
                 insert into bookless_position_live values
                   ('b', NULL, 1, 999, '2026-08-30T14:00:00Z');",
            )
            .unwrap();
        let gens = resolve_after_rebuild(
            &store,
            "ds",
            &[
                "bookless_position_archive".to_string(),
                "bookless_position_live".to_string(),
            ],
            ts("2026-08-30T10:00:00Z"),
        )
        .unwrap();
        assert_eq!(gens.len(), 1, "one partition, one resolved generation");
        assert_eq!(
            gens[0].book, None,
            "the bookless partition must resolve, not be filtered out"
        );
        assert_eq!(
            gens[0].source_time,
            ts("2026-08-30T07:00:00Z"),
            "the 07:00 archived generation, not 14:00 live"
        );

        let pred = generation_predicate(&gens);
        let relation = crate::query::scope_sql::Era {
            kind: crate::store::ddl::TableKind::Archive,
            generations: Some(&pred),
        }
        .relation("bookless", geode_core::schema::Grain::Position);
        let total: f64 = store
            .writer()
            .query_row(&format!("select sum(pnl) from {relation}"), [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            total, 100.0,
            "the archived NULL-book row's value, not live's 999 — the \
             IN-list alone (gen_id in (1)) cannot exclude live's row, which \
             shares gen_id 1; only the tuple's source_time term can"
        );
    }

    #[test]
    fn an_empty_resolution_selects_no_rows_rather_than_all() {
        assert_eq!(generation_predicate(&[]), "false");
    }

    #[test]
    fn resolve_reads_only_the_named_dataset() {
        // Two datasets sharing a (batch, book) with different, larger
        // generations: if the `where dataset = ?` filter were ever
        // dropped, resolving 'a' would see 'b's newer generation through
        // the same `partition by (batch, book)` window and answer wrong
        // rather than merely answering more.
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        crate::store::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        store
            .writer()
            .execute_batch(
                "insert into generations values
                   ('a', 'b', 'BK0', 1, '2026-08-30T07:00:00Z'),
                   ('b', 'b', 'BK0', 99, '2026-08-30T09:00:00Z');",
            )
            .unwrap();
        let gens = resolve_generations(store.writer(), "a", ts("2026-08-30T10:00:00Z")).unwrap();
        assert_eq!(gens.len(), 1);
        assert_eq!(
            gens[0].gen_id, 1,
            "dataset 'a's own generation, not 'b's newer one"
        );
    }

    #[test]
    fn resolve_generations_agrees_with_a_full_table_scan_across_four_instants() {
        // Compare summary resolution with a payload scan before, at, between,
        // and after the fixture's source times. Include a corrected republish
        // across live/archive, a bookless partition, and a generation present
        // at only one grain.
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        crate::store::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        store
            .writer()
            .execute_batch(
                "create table oracle_position_archive(
                     batch varchar, book varchar, gen_id bigint,
                     source_time timestamp with time zone);
                 create table oracle_position_live(
                     batch varchar, book varchar, gen_id bigint,
                     source_time timestamp with time zone);
                 create table oracle_underlying_live(
                     batch varchar, book varchar, gen_id bigint,
                     source_time timestamp with time zone);
                 -- BK000: a corrected republish tied at 07:00, straddling
                 -- archive (gen 1) and live (gen 2).
                 insert into oracle_position_archive values
                   ('BK000', 'BK000', 1, '2026-08-30T07:00:00Z');
                 insert into oracle_position_live values
                   ('BK000', 'BK000', 2, '2026-08-30T07:00:00Z'),
                   -- BK001: the bookless partition, published once, later.
                   ('BK001', NULL, 3, '2026-08-30T09:00:00Z');
                 -- BK002: a generation present *only* at the underlying
                 -- grain -- the whole reason `history_of` unions every
                 -- grain rather than resolving from the spine alone.
                 insert into oracle_underlying_live values
                   ('BK002', 'BK002', 4, '2026-08-30T09:30:00Z');",
            )
            .unwrap();
        let tables = [
            "oracle_position_archive".to_string(),
            "oracle_position_live".to_string(),
            "oracle_underlying_live".to_string(),
        ];
        crate::store::ddl::rebuild_generations(store.writer(), "risk_snapshot", &tables).unwrap();

        fn sorted(
            gens: Vec<ResolvedGeneration>,
        ) -> Vec<(String, Option<String>, i64, DateTime<Utc>)> {
            let mut v: Vec<_> = gens
                .into_iter()
                .map(|g| (g.batch, g.book, g.gen_id, g.source_time))
                .collect();
            v.sort();
            v
        }

        for at in [
            ts("2026-08-30T06:00:00Z"), // before all history
            ts("2026-08-30T07:00:00Z"), // the tie instant
            ts("2026-08-30T08:00:00Z"), // between the tie and BK001
            ts("2026-08-30T10:00:00Z"), // after everything, including BK002
        ] {
            let via_summary =
                sorted(resolve_generations(store.writer(), "risk_snapshot", at).unwrap());
            let via_tables = sorted(resolve_from_tables(store.writer(), &tables, at).unwrap());
            assert_eq!(via_summary, via_tables, "disagreement at {at}");
        }

        // The one-grain-only generation must actually resolve, not just
        // ride along inside an oracle equality that would pass just as
        // well if both sides silently dropped it.
        let after = sorted(
            resolve_generations(store.writer(), "risk_snapshot", ts("2026-08-30T10:00:00Z"))
                .unwrap(),
        );
        assert!(
            after.contains(&(
                "BK002".to_string(),
                Some("BK002".to_string()),
                4,
                ts("2026-08-30T09:30:00Z"),
            )),
            "BK002's underlying-only generation must resolve: {after:?}"
        );
    }
}
