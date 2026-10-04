//! The concrete topics each subscribed source has published from, pruned
//! and read at service open, before the ingest runner owns the writer.
//! Written in the publish transaction of a topic's first NOTIFY document
//! per run, and again once the receiver's record of it is 24 hours old, so
//! a long run keeps a live topic's receive time current. A recovery reply
//! also writes it: in the transaction when the reply publishes, and as one
//! upsert when it is unchanged, because an answered GET proves the topic
//! alive. A topic that neither publishes nor answers for the source's
//! `recover_max_age` is pruned.

use crate::store::StoreError;
use chrono::{DateTime, Utc};
use duckdb::Connection;
use std::time::Duration;

fn sql_err(statement: &str) -> impl FnOnce(duckdb::Error) -> StoreError + '_ {
    move |source| StoreError::Sql {
        statement: statement.to_string(),
        source,
    }
}

/// Record that `source` received a document on `topic` at `at`. A repeat
/// keeps the later receive time, so an out-of-order or replayed record
/// cannot age a live topic into the next prune.
pub fn record(
    conn: &Connection,
    source: &str,
    topic: &str,
    at: DateTime<Utc>,
) -> Result<(), StoreError> {
    let sql = "insert into subscription_topics values (?, ?, ?, ?) \
               on conflict (source, topic) do update set \
               last_received_us = greatest(last_received_us, excluded.last_received_us)";
    let us = at.timestamp_micros();
    conn.execute(sql, duckdb::params![source, topic, us, us])
        .map_err(sql_err(sql))?;
    Ok(())
}

/// Delete `source`'s topics last received before `now - max_age`, returning
/// how many went. Other sources' rows are untouched. A zero `max_age` sets
/// the cutoff at `now` and so deletes every row of the source received
/// before `now` — at open, every row: a zero recovery max age means
/// "recover nothing".
pub fn prune(
    conn: &Connection,
    source: &str,
    max_age: Duration,
    now: DateTime<Utc>,
) -> Result<usize, StoreError> {
    let sql = "delete from subscription_topics where source = ? and last_received_us < ?";
    let age = i64::try_from(max_age.as_micros()).unwrap_or(i64::MAX);
    let cutoff = now.timestamp_micros().saturating_sub(age);
    conn.execute(sql, duckdb::params![source, cutoff])
        .map_err(sql_err(sql))
}

/// Every topic still recorded for `source`, sorted so recovery requests go
/// out in a stable order.
pub fn recent(conn: &Connection, source: &str) -> Result<Vec<String>, StoreError> {
    let sql = "select topic from subscription_topics where source = ? order by topic";
    let mut stmt = conn.prepare(sql).map_err(sql_err(sql))?;
    let rows = stmt
        .query_map([source], |r| r.get::<_, String>(0))
        .map_err(sql_err(sql))?;
    rows.collect::<Result<_, _>>().map_err(sql_err(sql))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::catalog::Catalog;
    use chrono::TimeZone;

    fn conn() -> duckdb::Connection {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        Catalog::new(&conn).ensure_tables().unwrap();
        conn
    }
    fn t(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    #[test]
    fn a_recorded_topic_is_read_back_per_source() {
        let c = conn();
        record(&c, "cvi", "marketdata/cvi/SPX/NOTIFY", t(100)).unwrap();
        record(&c, "cvi", "marketdata/cvi/SX5E/NOTIFY", t(100)).unwrap();
        record(&c, "div", "marketdata/dividend/SPX/NOTIFY", t(100)).unwrap();
        assert_eq!(
            recent(&c, "cvi").unwrap(),
            vec!["marketdata/cvi/SPX/NOTIFY", "marketdata/cvi/SX5E/NOTIFY"]
        );
    }

    #[test]
    fn recording_again_keeps_the_later_receive_time() {
        let c = conn();
        record(&c, "cvi", "a/NOTIFY", t(500)).unwrap();
        record(&c, "cvi", "a/NOTIFY", t(100)).unwrap();
        // Pruning everything older than 400 keeps the row: 500 survived.
        assert_eq!(
            prune(&c, "cvi", Duration::from_secs(100), t(500)).unwrap(),
            0
        );
        assert_eq!(recent(&c, "cvi").unwrap(), vec!["a/NOTIFY"]);
    }

    #[test]
    fn prune_removes_only_this_sources_old_rows() {
        let c = conn();
        record(&c, "cvi", "old/NOTIFY", t(0)).unwrap();
        record(&c, "cvi", "new/NOTIFY", t(1_000)).unwrap();
        record(&c, "div", "old/NOTIFY", t(0)).unwrap();
        assert_eq!(
            prune(&c, "cvi", Duration::from_secs(500), t(1_000)).unwrap(),
            1
        );
        assert_eq!(recent(&c, "cvi").unwrap(), vec!["new/NOTIFY"]);
        assert_eq!(recent(&c, "div").unwrap(), vec!["old/NOTIFY"]);
    }

    #[test]
    fn a_zero_max_age_prunes_every_row_of_the_source() {
        let c = conn();
        record(&c, "cvi", "a/NOTIFY", t(1_000)).unwrap();
        record(&c, "cvi", "b/NOTIFY", t(999)).unwrap();
        // Received an instant before `now`: older than a zero max age.
        assert_eq!(prune(&c, "cvi", Duration::ZERO, t(1_001)).unwrap(), 2);
        assert!(recent(&c, "cvi").unwrap().is_empty());
    }
}
