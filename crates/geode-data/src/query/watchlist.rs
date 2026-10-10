//! One watchlist resolved live: a filtered distinct of `underlying_ref`
//! per rule, then the pure set algebra over the names.
//!
//! Each rule runs in its own read transaction. A statement that fails
//! inside a DuckDB transaction leaves it aborted, so one transaction for
//! every rule would let a rule failing at execution take the rules after
//! it, and the commit, down with it. Per-rule transactions isolate both a
//! compile failure and an execution failure to the rule's index in
//! `rules_failed`; the other rules still answer. The cost is that two rules
//! can read either side of a publish landing between them; a live list is
//! requeried on publication anyway, and the next answer agrees with itself.

use std::sync::Arc;

use duckdb::Connection;
use geode_core::query::{AsOf, DistinctParams, WatchlistParams, WatchlistResult};
use geode_core::watchlist::UNDERLYING;
use geode_core::watchlist::members::resolve_members;

use super::distinct::compile_distinct_with_cache;
use super::read::ReadConfig;
use super::scope_sql::DictionaryCache;
use crate::store::{StoreError, begin_transaction, commit_transaction};

#[derive(Debug)]
pub struct WatchlistQuery {
    config: Arc<ReadConfig>,
    params: WatchlistParams,
}

impl WatchlistQuery {
    pub(crate) fn new(config: Arc<ReadConfig>, params: WatchlistParams) -> Self {
        Self { config, params }
    }

    pub fn name(&self) -> &str {
        &self.params.name
    }

    /// `Err` is a failure of the connection itself (a transaction refused);
    /// a rule's own failure is in the result, by index.
    pub(crate) fn run(&self, conn: &Connection) -> Result<WatchlistResult, StoreError> {
        let mut rule_names: Vec<(usize, Vec<String>)> = Vec::new();
        let mut rules_failed: Vec<(usize, String)> = Vec::new();
        for rule in &self.params.rules {
            // Compilation never reads the key or tag; the result rides the
            // watchlist's own.
            let params = DistinctParams {
                key: self.params.key,
                tag: self.params.tag,
                column: UNDERLYING.to_string(),
                scope: rule.scope.clone(),
                as_of: AsOf::Live,
                dataset: Some(rule.dataset.clone()),
            };
            let tx = begin_transaction(conn)?;
            match run_distinct(&tx, &self.config, &params) {
                Ok(names) => {
                    commit_transaction(tx)?;
                    rule_names.push((rule.index, names));
                }
                // Dropping `tx` rolls the aborted transaction back; the
                // next rule begins a clean one.
                Err(reason) => rules_failed.push((rule.index, reason)),
            }
        }
        Ok(WatchlistResult {
            members: resolve_members(&rule_names, &self.params.include, &self.params.exclude),
            rules_failed,
        })
    }
}

/// One rule's names, compiled and run inside `tx`. Either failure is the
/// rule's, as text. The dictionary cache is fresh per statement:
/// publication rebuilds ENUM types, so a cache must not outlive its
/// transaction.
fn run_distinct(
    tx: &Connection,
    config: &ReadConfig,
    params: &DistinctParams,
) -> Result<Vec<String>, String> {
    let compiled = compile_distinct_with_cache(
        tx,
        &config.schema,
        &config.dimensions,
        params,
        &mut DictionaryCache::default(),
    )
    .map_err(|e| e.to_string())?;
    let mut stmt = tx.prepare(&compiled.sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(duckdb::params_from_iter(compiled.params.iter()), |row| {
            row.get::<_, String>(0)
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<String>, _>>()
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::distinct::tests::document_fixture;
    use geode_core::query::{QueryKey, ResolvedRule};
    use geode_core::scope::Scope;
    use geode_core::watchlist::members::Origin;

    // The distinct module's document fixture: `risk` at underlying grain
    // carries `underlying_ref`, with SPX.Z and NDX.Z in BK000 and RTY.Z in
    // BK001.

    fn params(rules: Vec<ResolvedRule>, include: &[&str], exclude: &[&str]) -> WatchlistParams {
        WatchlistParams {
            key: QueryKey(7),
            tag: 1,
            name: "t".into(),
            rules,
            include: include.iter().map(|s| s.to_string()).collect(),
            exclude: exclude.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn rule(index: usize, column: &str, value: &str) -> ResolvedRule {
        ResolvedRule {
            index,
            dataset: "risk".into(),
            scope: Scope::one(column, value),
        }
    }

    #[test]
    fn two_rules_union_include_adds_exclude_drops_and_a_bad_rule_is_reported() {
        let f = document_fixture();
        let config = Arc::new(ReadConfig {
            schema: Arc::new(f.schema.clone()),
            dimensions: f.dims.clone(),
        });
        let rules = vec![
            rule(0, "book", "BK000"),
            rule(1, "book", "BK001"),
            // A scope on a column `risk` lacks: compile fails for this rule only.
            rule(2, "no_such_column", "x"),
        ];
        let q = WatchlistQuery::new(config, params(rules, &["MANUAL"], &["SPX.Z"]));
        let result = q.run(f.conn()).unwrap();
        assert_eq!(result.rules_failed.len(), 1);
        assert_eq!(result.rules_failed[0].0, 2);
        let manual = result.members.iter().find(|m| m.name == "MANUAL").unwrap();
        assert_eq!(manual.origin, Origin::Manual);
        let excluded = result.members.iter().find(|m| m.is_excluded()).unwrap();
        assert_eq!(excluded.name, "SPX.Z");
        assert!(
            !excluded.rules().is_empty(),
            "the excluded name came from a rule"
        );
        // Every non-excluded, non-manual member names the rule(s) that produced it.
        assert!(
            result
                .members
                .iter()
                .filter(|m| !m.is_excluded() && m.name != "MANUAL")
                .all(|m| !m.rules().is_empty())
        );
        let names: Vec<&str> = result.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["MANUAL", "NDX.Z", "RTY.Z", "SPX.Z"]);
    }

    /// A rule the compiler refuses (an unknown column) is that rule's
    /// failure alone: the rule after it still answers.
    #[test]
    fn a_rule_failing_before_another_leaves_the_other_answering() {
        let f = document_fixture();
        let config = Arc::new(ReadConfig {
            schema: Arc::new(f.schema.clone()),
            dimensions: f.dims.clone(),
        });
        let rules = vec![
            rule(0, "book", "BK000"),
            rule(1, "no_such_column", "x"),
            rule(2, "book", "BK001"),
        ];
        let q = WatchlistQuery::new(config, params(rules, &[], &[]));
        let result = q.run(f.conn()).unwrap();
        assert_eq!(result.rules_failed.len(), 1);
        assert_eq!(result.rules_failed[0].0, 1);
        assert!(
            result.rules_failed[0].1.contains("no_such_column"),
            "{:?}",
            result.rules_failed[0].1
        );
        let rty = result.members.iter().find(|m| m.name == "RTY.Z").unwrap();
        assert_eq!(rty.origin, Origin::Rules(vec![2]));
        assert_eq!(result.members.len(), 3);
        // The connection is usable afterwards: nothing was left aborted.
        f.conn().execute_batch("select 1").unwrap();
    }

    /// A statement the compiler accepts but DuckDB rejects inside the
    /// rule's transaction (a text column compared with a number fails when
    /// the statement binds) is that rule's failure alone: the rule after it
    /// still answers, and the connection is clean afterwards.
    #[test]
    fn a_rule_failing_at_execution_leaves_the_next_rule_and_connection_clean() {
        use geode_core::scope::{CompareOp, Expr, Literal};
        let f = document_fixture();
        let config = Arc::new(ReadConfig {
            schema: Arc::new(f.schema.clone()),
            dimensions: f.dims.clone(),
        });
        let runtime_failure = ResolvedRule {
            index: 0,
            dataset: "risk".into(),
            scope: Scope {
                expression: Some(Expr::Compare {
                    column: UNDERLYING.into(),
                    op: CompareOp::Gt,
                    value: Literal::Num(1.0),
                }),
                ..Scope::default()
            },
        };
        let rules = vec![runtime_failure, rule(1, "book", "BK001")];
        let q = WatchlistQuery::new(config, params(rules, &[], &[]));
        let result = q.run(f.conn()).unwrap();
        assert_eq!(result.rules_failed.len(), 1, "{:?}", result.rules_failed);
        let (index, reason) = &result.rules_failed[0];
        assert_eq!(*index, 0);
        // DuckDB's refusal of the bound statement, not the compiler's.
        assert!(reason.contains("Binder Error"), "{reason}");
        let rty = result.members.iter().find(|m| m.name == "RTY.Z").unwrap();
        assert_eq!(rty.origin, Origin::Rules(vec![1]));
        assert_eq!(result.members.len(), 1);
        f.conn().execute_batch("select 1").unwrap();
    }

    #[test]
    fn a_rule_naming_an_unknown_dataset_fails_alone() {
        let f = document_fixture();
        let config = Arc::new(ReadConfig {
            schema: Arc::new(f.schema.clone()),
            dimensions: f.dims.clone(),
        });
        let rules = vec![
            ResolvedRule {
                index: 0,
                dataset: "nope".into(),
                scope: Scope::default(),
            },
            rule(1, "book", "BK001"),
        ];
        let q = WatchlistQuery::new(config, params(rules, &[], &[]));
        let result = q.run(f.conn()).unwrap();
        assert_eq!(result.rules_failed.len(), 1);
        assert_eq!(result.rules_failed[0].0, 0);
        assert!(
            result.rules_failed[0].1.contains("no dataset 'nope'"),
            "{}",
            result.rules_failed[0].1
        );
        let names: Vec<&str> = result.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["RTY.Z"]);
    }

    #[test]
    fn a_list_with_no_rules_is_its_manual_names() {
        let f = document_fixture();
        let config = Arc::new(ReadConfig {
            schema: Arc::new(f.schema.clone()),
            dimensions: f.dims.clone(),
        });
        let q = WatchlistQuery::new(config, params(vec![], &["SPX", "NDX"], &[]));
        let result = q.run(f.conn()).unwrap();
        assert_eq!(
            result
                .members
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>(),
            vec!["NDX", "SPX"]
        );
        assert!(result.rules_failed.is_empty());
    }
}
