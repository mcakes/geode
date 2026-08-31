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
# This script edits tracked source files in place and restores them
# afterwards, so it takes three precautions.
#
#   * The backup path is unique per run. A single shared /tmp path let two
#     concurrent runs restore each other's backup over the wrong file, and
#     one checkout ended up with the contents of scope_sql.rs inside
#     compile.rs.
#   * A lock directory serialises runs against the same checkout, because
#     two runs mutating the same files cannot both be meaningful anyway.
#   * A trap restores the file in flight however the script exits, so an
#     interrupt or a stale anchor cannot leave a mutation in the tree. An
#     earlier abort did exactly that, and the mutation was found committed
#     to a working tree days later.
#
# Usage: zsh scripts/mutation-check.sh [substring]   (from the repo root)
#
# With a substring, only entries whose name contains it are run — for
# iterating on the entries you just added. Always finish with an unfiltered
# run; a filtered one proves nothing about the rest.
set -e
cd "$(git rev-parse --show-toplevel)"

# One run at a time per checkout. mkdir is atomic, which is the whole
# requirement, and it needs no flock binary.
lock="$(git rev-parse --git-dir)/mutation-check.lock"
if ! mkdir "$lock" 2>/dev/null; then
  echo "another mutation-check is running in this checkout ($lock)" >&2
  echo "if that is stale: rmdir $lock" >&2
  exit 1
fi

bak="$(mktemp -t mutate-bak)"
log="$(mktemp -t mutate-log)"
in_flight=""

# Restore whatever is mutated right now, however we leave.
restore() {
  if [[ -n "$in_flight" ]]; then
    cp "$bak" "$in_flight"
    in_flight=""
  fi
}
cleanup() {
  restore
  rm -f "$bak" "$log"
  rmdir "$lock" 2>/dev/null || true
}
# A signal handler that merely returns lets the script carry on to the next
# mutation, which is not what anyone pressing ctrl-C means. Each signal
# handler restores and then exits; EXIT alone would not stop the run.
trap cleanup EXIT
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM

# run_mutation <name> <file> <from> <to> [package]
#
# `package` is the crate whose lib tests should see the mutation, and
# defaults to geode-data. It matters: a mutation in geode-core guarded only
# by a geode-core test is invisible to `-p geode-data`, so the entry would
# report "caught" on the strength of unrelated tests, or "SURVIVED" while a
# perfectly good test sits one crate away. Name the crate that holds the
# test, not the crate that holds the code.
only="${1:-}"

run_mutation() {
  local name="$1" file="$2" from="$3" to="$4" pkg="${5:-geode-data}"
  if [[ -n "$only" && "$name" != *"$only"* ]]; then
    return 0
  fi
  cp "$file" "$bak"
  in_flight="$file"
  local rc=0
  python3 - "$file" "$from" "$to" <<'PY' || rc=$?
import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
if sys.argv[2] not in s:
    print("ANCHOR-MISSING"); sys.exit(3)
p.write_text(s.replace(sys.argv[2], sys.argv[3], 1))
PY
  if (( rc != 0 )); then
    # A stale anchor is a finding in its own right: the mutation no longer
    # names live code. It is not a reason to abort mid-run with the tree
    # half-mutated.
    echo "ANCHOR    $name  <-- anchor no longer matches; mutation is stale"
    restore
    return 0
  fi
  if cargo test -p "$pkg" --lib >"$log" 2>&1; then
    echo "SURVIVED  $name  <-- no test sees this"
  else
    echo "caught    $name"
  fi
  restore
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

# ---- the snapshot read path (spec §6.6, §6.3)

run_mutation "snapshot: a null measure is not zero" \
  crates/geode-core/src/snapshot.rs \
  '(row < values.len() && !values.is_null(row)).then(|| values.value(row))' \
  '(row < values.len()).then(|| values.value(row))' \
  geode-core

run_mutation "probe: a blanked cell renders blank, not 0.00" \
  crates/geode-shell/src/dataprobe.rs \
  'return snap.f64_value(column, row).map(|v| format!("{v:.2}"));' \
  'return snap.f64_column(column)?.get(row).map(|v| format!("{v:.2}"));' \
  geode-shell

run_mutation "snapshot: a rolled-up dimension cell is null" \
  crates/geode-core/src/snapshot.rs \
  'if row >= d.len() || d.is_null(row) {' \
  'if row >= d.len() {' \
  geode-core

run_mutation "snapshot: dimension cells read at UInt16 key width" \
  crates/geode-core/src/snapshot.rs \
  'arr.as_any().downcast_ref::<DictionaryArray<UInt16Type>>()?,' \
  'None::<&DictionaryArray<UInt16Type>>?,' \
  geode-core

run_mutation "snapshot: dictionary columns expose UInt16 codes" \
  crates/geode-core/src/snapshot.rs \
  'let d = arr.as_any().downcast_ref::<DictionaryArray<UInt16Type>>()?;' \
  'let d = None::<&DictionaryArray<UInt16Type>>?;' \
  geode-core

# No entry for the UInt16 arm of concat_preserving_dictionaries. Removing
# it falls back to arrow's own concat, which — measured, not assumed —
# unifies the shared dictionary to the same result. It is a speed
# optimization at arrow 58.4.0, so a mutation of it has no behaviour for a
# test to catch, and an entry here would only ever report SURVIVED. The
# shared-dictionary path's actual behaviour is covered below.

run_mutation "snapshot: every batch contributes its dictionary keys" \
  crates/geode-core/src/snapshot.rs \
  'let keys: Vec<&dyn Array> = dicts.iter().map(|d| d.keys() as &dyn Array).collect();' \
  'let keys: Vec<&dyn Array> = dicts[..1].iter().map(|d| d.keys() as &dyn Array).collect();' \
  geode-core

run_mutation "snapshot: text reads a dimension under either era encoding" \
  crates/geode-core/src/snapshot.rs \
  '        self.dict_value(name, row)
            .or_else(|| self.str_value(name, row))' \
  '        self.str_value(name, row)' \
  geode-core
