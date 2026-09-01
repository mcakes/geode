//! Time travel (spec §4.5, §6.5). The same compiled SQL, aimed at the
//! archive tables, with the generation resolved per partition.
//!
//! Datasets and books refresh on independent cadences, so the resolved
//! state is "each partition as it stood at T" — the question a trader is
//! actually asking. The live path carries no generation predicate at all;
//! only this path pays for history.

use crate::store::StoreError;
use chrono::{DateTime, Utc};
use duckdb::Connection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsOf {
    Live,
    At(DateTime<Utc>),
}

impl AsOf {
    pub fn is_live(&self) -> bool {
        matches!(self, AsOf::Live)
    }
}

/// One partition's state at a point in time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedGeneration {
    pub batch: String,
    /// `Option` because ingest permits rows with no book: it reports them
    /// as a degradation rather than dropping them (spec §4.4), so a
    /// NULL-book partition is a real partition and time travel has to be
    /// able to name it. `retention.rs` handles the same hazard the same way.
    pub book: Option<String>,
    pub gen_id: i64,
    /// When this generation was published. The honest label for a
    /// historical result: the requested instant is what the user asked
    /// for, not what the data actually is.
    pub source_time: DateTime<Utc>,
}

/// The newest generation at or before `at`, for every partition that
/// existed by then, across **all** of the tables a dataset's history
/// lives in — every grain's archive *and* live.
///
/// It must be all of them, in both directions. A generation is a *file*,
/// and one file publishes every grain under a single `gen_id` — but a
/// partition can be absent from one grain's archive while present at
/// another (a cash-only book has no underlying rows; a grain added later
/// has no history at all while coarser grains have years). And the
/// generation a partition holds *now* is in live and nowhere else: the
/// publish transaction moves the outgoing generation to the archive, it
/// does not copy the incoming one there (§4.3). Resolving from the archive
/// alone answers "as of an hour ago" with this morning's *previous* file,
/// and finds nothing at all for a partition published only once. Either
/// omission narrows the answer silently, the same class of defect as
/// matching a NULL book with `=`.
///
/// Ties on `source_time` break on `gen_id`, newest first. They are
/// ordinary: a corrected republish keeps its source time (§4.4) and the
/// generation it replaced goes to the archive with the same stamp, so the
/// archive holds two generations of one partition at one instant. Without
/// the tiebreak the window function's choice is whatever order the rows
/// came back in, and `retention.rs` can keep the one this drops.
pub fn resolve_generations(
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
    // Propagated, not swallowed. Discarding a row here narrows the
    // resolved generation set, which silently narrows the *result* — a
    // query that answers with less data than it should and says nothing.
    rows.collect::<Result<Vec<_>, _>>().map_err(err)
}

/// A predicate selecting exactly those generations. Values come from the
/// catalog, not from user input, so they are inlined as quoted literals;
/// scope predicates, which do take user input, bind (spec §6.2).
///
/// A generation is named by `(batch, book, gen_id, source_time)`, not by
/// `gen_id` alone. The relation this filters is archive-plus-live, and a
/// `gen_id` collision would otherwise select two generations at once.
///
/// `gen_id` now comes from a sequence (`Catalog::reserve_gen_id`), so a
/// load that publishes and then fails to record can no longer leave its id
/// for the next load to reuse. **The source-time term stays anyway**, and
/// the prerequisites doc's suggestion that it could go once the sequence
/// landed is wrong on one point: the sequence fixes allocation from here
/// on, and does nothing about ids already written. A database loaded by an
/// older build can hold two generations of one partition sharing an id
/// right now, and dropping this term would make those ambiguous again —
/// silently, and only for the history that predates the fix.
///
/// The cost is real but small: statement text grows with partition count,
/// which is on the deferred list as a plan-caching concern. Correctness on
/// existing data outranks it.
pub fn generation_predicate(generations: &[ResolvedGeneration]) -> String {
    if generations.is_empty() {
        // Selecting nothing, not everything: a time before all history is
        // an empty result, never the whole archive.
        return "false".to_string();
    }
    generations
        .iter()
        .map(|g| {
            let (batch, book, generation) = (&g.batch, &g.book, g.gen_id);
            // `book = '…'` cannot match a NULL book, so a partition with
            // no book would be silently excluded from every historical
            // answer while appearing in the live one.
            let book_term = match book {
                Some(b) => format!("book = '{}'", b.replace('\'', "''")),
                None => "book is null".to_string(),
            };
            format!(
                "(batch = '{}' and {book_term} and gen_id = {generation} \
                 and source_time = '{}'::timestamptz)",
                batch.replace('\'', "''"),
                g.source_time.to_rfc3339(),
            )
        })
        .collect::<Vec<_>>()
        .join(" or ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// An archive with two partitions refreshing on different clocks:
    /// BK000 at 07:00 and 14:00, BK001 only at 09:00.
    fn fixture() -> (tempfile::TempDir, crate::store::Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
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
        let gens = resolve_generations(
            store.writer(),
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
        // Books refresh independently, so 'as it stood at T' is per
        // partition, not one dataset-wide generation (spec §4.5).
        let (_d, store) = fixture();
        let gens = resolve_generations(
            store.writer(),
            &["risk_snapshot_position_archive".to_string()],
            ts("2026-08-30T08:00:00Z"),
        )
        .unwrap();
        assert_eq!(gens.len(), 1, "BK001 did not exist yet at 08:00: {gens:?}");
        assert_eq!(gens[0].book.as_deref(), Some("BK000"));
    }

    #[test]
    fn a_row_that_cannot_be_read_is_an_error_not_a_smaller_answer() {
        // Swallowing a decode error here silently narrows the resolved
        // generation set, which silently narrows the *result*: a query
        // that answers with less data than it has and says nothing. A
        // loud failure is the only honest outcome.
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
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
        let err = resolve_generations(
            store.writer(),
            &["bad_archive".to_string()],
            ts("2026-08-30T10:00:00Z"),
        );
        assert!(err.is_err(), "an unreadable generation row must propagate");
    }

    #[test]
    fn a_tie_on_source_time_resolves_to_the_newest_gen_id_every_time() {
        // A corrected republish keeps its source time (§4.4), so two
        // generations of one partition share an instant. Without a
        // tiebreak the window function's pick is whichever row came back
        // first.
        //
        // Eight tied generations, not two, and the winner is the one
        // inserted *first*. The loop below is nearly free as a detector on
        // its own: the plan is deterministic within a process, so twenty
        // iterations sample the same answer twenty times rather than
        // twenty times independently. What makes this catch the missing
        // tiebreak is the fixture — an unordered pick takes the scan's
        // first row, which here is the lowest `gen_id`, so the wrong
        // answer is wrong every time instead of half the time. Measured:
        // with two rows the mutation survived two runs in three.
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
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
        for _ in 0..20 {
            let gens = resolve_generations(
                store.writer(),
                &["risk_snapshot_position_archive".to_string()],
                ts("2026-08-30T10:00:00Z"),
            )
            .unwrap();
            assert_eq!(gens.len(), 1);
            assert_eq!(gens[0].gen_id, 8, "the correction wins, deterministically");
        }
    }

    #[test]
    fn the_predicate_names_the_source_time_so_a_reused_gen_id_selects_one_generation() {
        // `gen_id` is `max + 1` over the catalog, allocated before the
        // catalog row is written: a publish that then fails to record
        // leaves rows whose id the next load reuses. Same partition, same
        // id, different instants — the predicate must pick one.
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
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
        let gens = resolve_generations(
            store.writer(),
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
        let gens = resolve_generations(
            store.writer(),
            &["risk_snapshot_position_archive".to_string()],
            ts("2026-08-29T00:00:00Z"),
        )
        .unwrap();
        assert!(gens.is_empty());
    }

    #[test]
    fn the_predicate_selects_exactly_the_resolved_generations() {
        let (_d, store) = fixture();
        let gens = resolve_generations(
            store.writer(),
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
    fn an_empty_resolution_selects_no_rows_rather_than_all() {
        assert_eq!(generation_predicate(&[]), "false");
    }
}
