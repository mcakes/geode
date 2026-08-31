#!/bin/zsh
#
# Mutation check for the query path (spec §6).
#
# Each entry breaks one load-bearing behaviour and runs the suite. A
# mutation that SURVIVES is a branch no test can see — the suite is green
# whether that code is right or wrong.
#
# This exists because four rounds of code review found silent defects the
# suite could not see, and the fixture was the reason every time: reviews
# find what the fixture makes reachable. Reading the tests never revealed
# that; twenty minutes of mutation did. Run it after touching the
# compiler, the scope lowering, or as-of routing, and treat a SURVIVED
# line as a missing test rather than a curiosity.
#
# Usage: zsh scripts/mutation-check.sh   (from the repo root)
set -e
cd "$(git rev-parse --show-toplevel)"

run_mutation() {
  local name="$1" file="$2" from="$3" to="$4"
  cp "$file" /tmp/mutate.bak
  python3 - "$file" "$from" "$to" <<'PY'
import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
if sys.argv[2] not in s:
    print("ANCHOR-MISSING"); sys.exit(3)
p.write_text(s.replace(sys.argv[2], sys.argv[3], 1))
PY
  if cargo test -p geode-data --lib >/tmp/mutate.log 2>&1; then
    echo "SURVIVED  $name  <-- no test sees this"
  else
    echo "caught    $name"
  fi
  cp /tmp/mutate.bak "$file"
}

run_mutation "as-of: multi-grain generation resolution" \
  crates/geode-data/src/query/compile.rs \
  '&archives_of(&view.dataset, ds),' \
  '&[table_name(&view.dataset, spine_grain, TableKind::Archive)],'

run_mutation "as-of: probe generation predicate" \
  crates/geode-data/src/query/scope_sql.rs \
  'if let Some(generations) = era.generations {' \
  'if let Some(generations) = None::<&str> {'

run_mutation "as-of: probe table kind" \
  crates/geode-data/src/query/scope_sql.rs \
  'table_name(&ds.name, probe, era.kind),' \
  'table_name(&ds.name, probe, TableKind::Live),'

run_mutation "as-of: ENUM cast era guard" \
  crates/geode-data/src/query/compile.rs \
  'if era.kind != TableKind::Live {' \
  'if false {'

run_mutation "as-of: NULL book predicate" \
  crates/geode-data/src/query/as_of.rs \
  'None => "book is null".to_string(),' \
  'None => "book = %".to_string(),'

run_mutation "as-of: error propagation" \
  crates/geode-data/src/query/as_of.rs \
  'rows.collect::<Result<Vec<_>, _>>().map_err(err)' \
  'Ok(rows.filter_map(|r| r.ok()).collect())'

run_mutation "provenance: resolved vs requested time" \
  crates/geode-data/src/service.rs \
  'as_of: compiled' \
  'as_of: None.or(compiled'

run_mutation "scope: is_finer excludes same-grain measures" \
  crates/geode-data/src/query/scope_sql.rs \
  'match ds.column(base).and_then(|col| col.grain()) {' \
  'match None::<Grain>.map(|g: Grain| g) {'

run_mutation "scope: single-pass param assembly" \
  crates/geode-data/src/query/scope_sql.rs \
  'let finer_params: Vec<Value> = finer.iter().flat_map(|(_, p)| p.clone()).collect();' \
  'let finer_params: Vec<Value> = Vec::new();'

run_mutation "join: aggregate to the join key" \
  crates/geode-data/src/query/compile.rs \
  'group by {keys}) {alias} on {on}' \
  ') {alias} on {on}'

run_mutation "aggregate: sub_depth level guard" \
  crates/geode-data/src/query/compile.rs \
  '.chain(std::iter::once(level))' \
  ''

run_mutation "derived: scalar projection" \
  crates/geode-data/src/query/compile.rs \
  '    if derived.is_empty() {' \
  '    if true {'
