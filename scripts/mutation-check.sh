#!/bin/zsh
#
# Mutation check for the query path (spec §6).
#
# Each entry breaks one load-bearing behaviour and runs the suite. A
# mutation that SURVIVES is a branch no test can see — the suite is green
# whether that code is right or wrong.
#
# This exists because five rounds of code review found silent defects the
# suite could not see, and the fixture was the reason every time: reviews
# find what the fixture makes reachable. Reading the tests never revealed
# that; twenty minutes of mutation did. Run it after touching the
# compiler, the scope lowering, as-of routing, publish, or the grain
# vocabulary, and treat a SURVIVED line as a missing test rather than a
# curiosity.
#
# Two entries (the source-time tie-breaks) are caught probabilistically:
# the defect is nondeterminism, and the tests loop twenty times.
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

# ---- as-of routing (spec §6.5)

run_mutation "as-of: multi-grain generation resolution" \
  crates/geode-data/src/query/compile.rs \
  '&history_of(&view.dataset, ds), *t)' \
  '&history_of(&view.dataset, ds)[2..], *t)'

run_mutation "as-of: current generation read from live" \
  crates/geode-data/src/query/scope_sql.rs \
  '"(select * from {} union all select * from {})"' \
  '"(select * from {} union all select * from {} where false)"'

run_mutation "as-of: probe generation predicate" \
  crates/geode-data/src/query/scope_sql.rs \
  'if let Some(generations) = era.generations {' \
  'if let Some(generations) = None::<&str> {'

run_mutation "as-of: probe era" \
  crates/geode-data/src/query/scope_sql.rs \
  'era.relation(&ds.name, probe),' \
  'table_name(&ds.name, probe, TableKind::Live),'

run_mutation "as-of: ENUM cast era guard" \
  crates/geode-data/src/query/compile.rs \
  'if era.kind != TableKind::Live {' \
  'if false {'

run_mutation "as-of: NULL book predicate" \
  crates/geode-data/src/query/as_of.rs \
  'None => "book is null".to_string(),' \
  'None => "book = %".to_string(),'

run_mutation "as-of: predicate names the source time" \
  crates/geode-data/src/query/as_of.rs \
  "and source_time = '{}'::timestamptz)" \
  "and '{}' is not null)"

run_mutation "as-of: source-time tie breaks on gen_id (probabilistic)" \
  crates/geode-data/src/query/as_of.rs \
  'order by source_time desc, gen_id desc' \
  'order by source_time desc'

run_mutation "retention: source-time tie breaks on gen_id (probabilistic)" \
  crates/geode-data/src/store/retention.rs \
  'order by source_time desc, gen_id desc' \
  'order by source_time desc'

run_mutation "as-of: error propagation" \
  crates/geode-data/src/query/as_of.rs \
  'rows.collect::<Result<Vec<_>, _>>().map_err(err)' \
  'Ok(rows.filter_map(|r| r.ok()).collect())'

run_mutation "provenance: resolved vs requested time" \
  crates/geode-data/src/service.rs \
  'as_of: compiled' \
  'as_of: None.or(compiled'

run_mutation "provenance: stalest partition, not newest" \
  crates/geode-data/src/query/compile.rs \
  'if let Some(oldest) = gens.iter().map(|g| g.source_time).min() {' \
  'if let Some(oldest) = gens.iter().map(|g| g.source_time).max() {'

# ---- the grain vocabulary (spec §3.3, §6.3)

run_mutation "vocabulary: pair grain does not carry the underlying dimension" \
  crates/geode-core/src/schema/grain.rs \
  'Grain::UnderlyingPair => &K_INSTRUMENT,' \
  'Grain::UnderlyingPair => &K_PAIR,'

run_mutation "attribution: decided on dimension keys" \
  crates/geode-core/src/attribution.rs \
  'let key = grain.dimension_key_columns();' \
  'let key = grain.key_columns();'

# ---- scope lowering (spec §6.2, §6.3)

run_mutation "scope: a same-grain measure is direct" \
  crates/geode-data/src/query/scope_sql.rs \
  '|| ds.column(base).and_then(|c| c.grain()) == Some(grain)' \
  '|| false'

run_mutation "scope: probe keys are the shared dimension keys" \
  crates/geode-data/src/query/scope_sql.rs \
  '.filter(|k| probe.dimension_key_columns().contains(k))' \
  '.filter(|k| probe.key_columns().contains(k))'

run_mutation "scope: single-pass param assembly" \
  crates/geode-data/src/query/scope_sql.rs \
  'let inner_params: Vec<Value> = mine.iter().flat_map(|(_, p)| p.clone()).collect();' \
  'let inner_params: Vec<Value> = Vec::new();'

run_mutation "scope: text filter reaches other grains" \
  crates/geode-data/src/query/scope_sql.rs \
  'terms.push(membership(ds, grain, probe, era, &test));' \
  '{ let _ = probe; continue; }'

run_mutation "scope: LIKE wildcards escaped" \
  crates/geode-data/src/query/scope_sql.rs \
  'if matches!(ch,' \
  'if false && matches!(ch,'

run_mutation "scope: conjuncts routed separately" \
  crates/geode-data/src/query/scope_sql.rs \
  'for term in conjuncts(expr) {' \
  'for term in [expr] {'

# ---- the compiler (spec §6.3, §6.4, §6.8)

run_mutation "spine: assembled from every aggregate, not the finest" \
  crates/geode-data/src/query/compile.rs \
  '.filter(|d| own_present[*d] == *d).collect();' \
  '.filter(|d| own_present[*d] == *d && own.len() == depth).collect();'

run_mutation "spine: the grand total row is constant" \
  crates/geode-data/src/query/compile.rs \
  'spine_sources.join(" union all ")' \
  'spine_sources[1..].join(" union all ")'

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

# ---- publish (spec §4.3, §4.4)

run_mutation "publish: the bookless partition is replaced" \
  crates/geode-data/src/store/publish.rs \
  'None => "book is null".to_string(),' \
  'None => "book = %".to_string(),'

run_mutation "ingest: the bookless partition is published" \
  crates/geode-data/src/ingest/load.rs \
  '.chain((unattributed_rows > 0).then_some(None))' \
  '.chain(None)'
