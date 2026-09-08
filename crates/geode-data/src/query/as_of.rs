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

/// Re-exported from `geode-core` (spec §2.7): the shell holds the frame's
/// as-of and cannot name this crate.
pub use geode_core::query::AsOf;

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
/// The shape is a `gen_id` IN-list plus a tuple semi-join, not the OR-chain
/// this used to emit (docs/perf.md, "Phase 4a: the as-of baseline"), and not
/// the `gen_id` range that briefly replaced the OR-chain and was itself
/// replaced here (docs/perf.md, "the range prefilter degenerates on a real
/// archive"). The facts that make the IN-list a straight win:
///
/// - `gen_id in (…)` is a plain scan filter DuckDB pushes to the table
///   scan as an *optional* filter; zonemaps then skip whole row groups
///   holding none of the listed ids — per id, not per range — and skip an
///   entire *side* of the union when it holds none of them at all (a
///   pure-archive era's live side, or a pure-live era's archive side). A
///   range (`between lo and hi`) pruned the same way only when the
///   resolved ids happened to sit close together; on a real desk, books
///   refresh on independent schedules (§4.5) — one last published
///   Tuesday, another an hour ago — so `[lo, hi]` widens toward the whole
///   archive and the range prunes nothing at all. Measured on a 20.8 GiB
///   demo database that had drifted to 3051 generations: a range spanning
///   `1..3051` scanned all 116M archived rows of one grain; the IN-list of
///   the same 17 resolved ids, on the same table, took 1.5 ms (docs/perf.md,
///   "the range prefilter degenerates on a real archive").
/// - The `in (select … from (values …))` tuple test is a hash lookup
///   evaluated once per row, replacing a per-row disjunction over up to
///   one term per resolved generation.
/// - DuckDB's row-value (struct) comparison treats NULL fields as equal
///   (`select (1, NULL::varchar) in (select (1, NULL::varchar))` is
///   `true`), so a NULL book needs no `coalesce` sentinel: `NULL::varchar`
///   in the `values` row matches a NULL `book` column exactly, which is
///   the bookless partition's whole correctness requirement.
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
    fn a_tie_that_straddles_archive_and_live_resolves_to_the_live_one() {
        // The fixture gap the phase-2b handoff named: every other tie test
        // puts both generations in the archive, so the tie-break was only
        // ever exercised *within* one relation.
        //
        // The real shape is different. A corrected republish keeps its
        // source time (§4.4), and the publish transaction moves the
        // outgoing generation to the archive while the incoming one stays
        // in live — so the two tied generations sit in *different*
        // relations, and `Era::relation` reads them as `archive union all
        // live`. Losing the tie-break across that union answers with the
        // superseded copy of a corrected file: the wrong numbers,
        // silently, for exactly the file someone corrected because it was
        // wrong. Verified to fail without the `gen_id desc` term.
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
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

        let gens = resolve_generations(
            store.writer(),
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
    fn the_predicate_names_a_gen_id_in_list_and_no_or_chain() {
        // Cheap and load-bearing: anchors the harness against a
        // regression back to the per-generation OR chain, and against an
        // IN-list that does not actually name the resolved ids. A range
        // (the form this replaced) degenerates on a real archive: books
        // refresh independently, so `[lo, hi]` widens toward the whole
        // archive and prunes nothing (docs/perf.md, "the range prefilter
        // degenerates on a real archive").
        let (_d, store) = fixture();
        let gens = resolve_generations(
            store.writer(),
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
        // A NULL-book generation in the archive, and a *later* generation
        // of the same bookless partition in live sharing the **same**
        // `gen_id`. Resolving at an instant before the live generation's
        // source time must pick the archived one — and the predicate,
        // applied to exactly the relation `Era::relation` builds (archive
        // union all live, each side filtered), must select the archived
        // row's value and not live's.
        //
        // The shared `gen_id` is load-bearing, not incidental: with only
        // one resolved generation, the IN-list term `gen_id in (1)`
        // cannot tell the two rows apart — both carry `gen_id = 1`. Only
        // the tuple's `source_time` column can, so this fixture makes the
        // tuple do real work rather than merely riding along behind an
        // IN-list that already excludes live on its own (a distinct
        // `gen_id` for live would let the IN-list alone pass this test).
        // `book = '…'` cannot match a NULL book;
        // DuckDB's row-value (struct) comparison treats NULL fields as
        // equal, so the tuple form needs no `coalesce` sentinel (spec
        // §4.4, §6.5).
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
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
        let gens = resolve_generations(
            store.writer(),
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
}
