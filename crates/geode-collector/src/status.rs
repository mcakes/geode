//! `geode-collector status`: who holds the store, read from the lock files.

use std::path::Path;

use geode_data::lease::{app_present, collector_present};

/// `collector: running|not running; app: present|absent; store: <db>`.
///
/// Each half is a lock probe (`try_lock`, then unlock at once), so a status
/// call never holds either lock for longer than an instant; a collector
/// starting during that instant retries its lease. A lock file that cannot
/// be probed reads `unknown (<error>)`.
pub fn status(db: &Path) -> String {
    let probe = |present: std::io::Result<bool>, yes: &str, no: &str| match present {
        Ok(true) => yes.to_string(),
        Ok(false) => no.to_string(),
        Err(err) => format!("unknown ({err})"),
    };
    format!(
        "collector: {}; app: {}; store: {}",
        probe(collector_present(db), "running", "not running"),
        probe(app_present(db), "present", "absent"),
        db.display()
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use geode_data::lease::{acquire_app, try_collector};

    use super::*;

    #[test]
    fn no_locks_is_no_collector_and_no_app() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("g.duckdb");
        assert_eq!(
            status(&db),
            format!(
                "collector: not running; app: absent; store: {}",
                db.display()
            )
        );
    }

    #[test]
    fn held_leases_read_running_and_present() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("g.duckdb");
        let collector = try_collector(&db).unwrap().expect("collector lease");
        assert_eq!(
            status(&db),
            format!("collector: running; app: absent; store: {}", db.display())
        );
        let (app, ()) = acquire_app(
            &db,
            Duration::from_secs(1),
            &|| false,
            &mut |_| {},
            &mut || Ok(()),
        )
        .expect("app lease");
        assert_eq!(
            status(&db),
            format!("collector: running; app: present; store: {}", db.display())
        );
        drop(collector);
        assert_eq!(
            status(&db),
            format!(
                "collector: not running; app: present; store: {}",
                db.display()
            )
        );
        drop(app);
    }
}
