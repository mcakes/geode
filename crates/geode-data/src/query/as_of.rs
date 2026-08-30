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

/// `(batch, book, gen_id)` — the newest generation at or before `at`, for
/// every partition that existed by then.
///
/// `book` is `Option` because ingest permits rows with no book: it reports
/// them as a degradation rather than dropping them (spec §4.4), so a
/// NULL-book partition is a real partition and time travel has to be able
/// to name it. `retention.rs` handles the same hazard the same way.
pub fn resolve_generations(
    conn: &Connection,
    archive_table: &str,
    at: DateTime<Utc>,
) -> Result<Vec<(String, Option<String>, i64)>, StoreError> {
    let sql = format!(
        "select batch, book, gen_id from (
             select batch, book, gen_id, source_time,
                    row_number() over (
                        partition by batch, book order by source_time desc
                    ) as rn
             from (select distinct batch, book, gen_id, source_time
                   from {archive_table}
                   where source_time <= ?)
         ) where rn = 1"
    );
    let err = |source| StoreError::Sql {
        statement: sql.clone(),
        source,
    };
    let mut stmt = conn.prepare(&sql).map_err(err)?;
    let rows = stmt
        .query_map(duckdb::params![at], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, i64>(2)?,
            ))
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
pub fn generation_predicate(generations: &[(String, Option<String>, i64)]) -> String {
    if generations.is_empty() {
        // Selecting nothing, not everything: a time before all history is
        // an empty result, never the whole archive.
        return "false".to_string();
    }
    generations
        .iter()
        .map(|(batch, book, generation)| {
            // `book = '…'` cannot match a NULL book, so a partition with
            // no book would be silently excluded from every historical
            // answer while appearing in the live one.
            let book_term = match book {
                Some(b) => format!("book = '{}'", b.replace('\'', "''")),
                None => "book is null".to_string(),
            };
            format!(
                "(batch = '{}' and {book_term} and gen_id = {generation})",
                batch.replace('\'', "''"),
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
            "risk_snapshot_position_archive",
            ts("2026-08-30T10:00:00Z"),
        )
        .unwrap();
        let bk000 = gens.iter().find(|(b, ..)| b == "BK000").unwrap();
        assert_eq!(bk000.2, 1, "07:00, not the 14:00 generation");
        let bk001 = gens.iter().find(|(b, ..)| b == "BK001").unwrap();
        assert_eq!(bk001.2, 3);
    }

    #[test]
    fn each_partition_resolves_on_its_own_clock() {
        // Books refresh independently, so 'as it stood at T' is per
        // partition, not one dataset-wide generation (spec §4.5).
        let (_d, store) = fixture();
        let gens = resolve_generations(
            store.writer(),
            "risk_snapshot_position_archive",
            ts("2026-08-30T08:00:00Z"),
        )
        .unwrap();
        assert_eq!(gens.len(), 1, "BK001 did not exist yet at 08:00: {gens:?}");
        assert_eq!(gens[0].0, "BK000");
    }

    #[test]
    fn a_time_before_all_history_resolves_to_nothing() {
        let (_d, store) = fixture();
        let gens = resolve_generations(
            store.writer(),
            "risk_snapshot_position_archive",
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
            "risk_snapshot_position_archive",
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
