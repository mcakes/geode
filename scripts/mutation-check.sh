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
# The two source-time tie-break entries were described as "caught
# probabilistically, because the tests loop twenty times". Measured, that
# reasoning was wrong: the query plan is deterministic within a process,
# so twenty iterations sample one answer twenty times rather than twenty
# times independently. The as-of entry survived two runs in three.
#
# What fixes it is the fixture, not the loop. Eight tied generations
# instead of two, with the winner inserted first, makes an unordered pick
# land on the wrong row every time rather than half the time: 4/4 caught
# after, 1/3 before. The retention entry measured 3/3 as it stood and was
# left alone. Neither is probabilistic now — treat a SURVIVED on either as
# a real finding.
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
# Usage: zsh scripts/mutation-check.sh [--changed[=REF]] [substring]
#   (from the repo root)
#
# With a substring, only entries whose name contains it are run — for
# iterating on the entries you just added. Always finish with an unfiltered
# run; a filtered one proves nothing about the rest. The unfiltered run is
# what CI and a merge gate should use; --changed is the everyday form
# while a change is in flight.
#
# --changed (default REF: main) skips any entry whose `file` is not among
# the files changed versus REF, changed in the working tree, or untracked.
# The changed set is computed once, before the first mutation — this
# script edits tracked files in place, so computing it later would see
# its own mutations. A run prints "skipped N entries whose files are
# unchanged since REF" at the end. --changed and a substring compose:
# `--changed "pool:"` runs only entries matching both filters.
#
# Cost note (measured on this checkout, warm cache): a geode-data entry
# with a covering test filter ("pool: the tag is echoed, not
# regenerated", filtered to the one test) took ~3s; the same entry with
# no filter, running the whole geode-data --lib suite, took ~36s — the
# filter is why the Phase 3a entries above name one. `.cargo/config.toml`
# pinning `profile.dev.split-debuginfo = "unpacked"` was also tried, to
# skip dsymutil packing on macOS: measured with `time` across two warm
# runs of a single entry, before (~3.1-3.4s) and after (~3.0-3.1s) adding
# the file — no measurable difference on this toolchain (cargo's macOS
# default is already "unpacked"), so the file was dropped rather than
# kept for a change that does nothing here.
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

# run_mutation <name> <file> <from> <to> [package] [test_filter]
#
# `package` is the crate whose lib tests should see the mutation, and
# defaults to geode-data. It matters: a mutation in geode-core guarded only
# by a geode-core test is invisible to `-p geode-data`, so the entry would
# report "caught" on the strength of unrelated tests, or "SURVIVED" while a
# perfectly good test sits one crate away. Name the crate that holds the
# test, not the crate that holds the code.
#
# `test_filter`, if given, is a `cargo test` name filter for the test(s)
# expected to catch this mutation, tried before the full crate suite:
#   - the filtered run fails            -> "caught    $name"
#   - the filtered run passes and the
#     full suite then fails             -> "caught*   $name  <-- caught by
#                                           a test other than '$test_filter'"
#     (the "caught for the wrong reason" case the header above warns
#     about, now visible — fix it by naming the right test)
#   - the filtered run passes and the
#     full suite also passes            -> "SURVIVED  $name  <-- no test
#                                           sees this"
#   - the filter matches zero tests
#     (cargo prints "running 0 tests")  -> "FILTER    $name  <-- '$test_filter'
#                                           matches no test", then falls
#                                           back to the full suite for a
#                                           plain caught/SURVIVED verdict
# Omitting `test_filter` keeps the old behaviour: run the full crate suite.
changed_ref=""
if [[ "${1:-}" == --changed ]]; then
  changed_ref="main"
  shift
elif [[ "${1:-}" == --changed=* ]]; then
  changed_ref="${1#--changed=}"
  shift
fi
only="${1:-}"
skipped=0
changed_files=""
if [[ -n "$changed_ref" ]]; then
  # Computed once, now, before any entry mutates a file — the harness
  # edits tracked files in place, so computing this later would see its
  # own mutations rather than the branch's real changes.
  changed_files=$(
    git diff --name-only "$changed_ref" --
    git diff --name-only
    git ls-files --others --exclude-standard
  )
fi

run_mutation() {
  local name="$1" file="$2" from="$3" to="$4" pkg="${5:-geode-data}" filter="${6:-}"
  if [[ -n "$only" && "$name" != *"$only"* ]]; then
    return 0
  fi
  if [[ -n "$changed_ref" ]] && ! printf '%s\n' "$changed_files" | grep -qxF "$file"; then
    skipped=$((skipped + 1))
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
  if [[ -n "$filter" ]]; then
    if cargo test -p "$pkg" --lib -- "$filter" >"$log" 2>&1; then
      if grep -q "running 0 tests" "$log"; then
        echo "FILTER    $name  <-- '$filter' matches no test"
        filter=""
      fi
    else
      echo "caught    $name"
      restore
      return 0
    fi
  fi
  if [[ -n "$filter" ]]; then
    # The named test passed despite the mutation; the full suite is the
    # real verdict, and a failure here means some other test caught it.
    if cargo test -p "$pkg" --lib >"$log" 2>&1; then
      echo "SURVIVED  $name  <-- no test sees this"
    else
      echo "caught*   $name  <-- caught by a test other than '$filter'"
    fi
  else
    if cargo test -p "$pkg" --lib >"$log" 2>&1; then
      echo "SURVIVED  $name  <-- no test sees this"
    else
      echo "caught    $name"
    fi
  fi
  restore
}

# ---- discovery (spec §5.2, §5.7)
#
# This decides what gets ingested at all, and its failure mode is the
# quietest in the system: a file that is never loaded produces no error,
# no degradation and no row — the tile is simply missing data nobody
# asked about. It had no entries.

run_mutation "discovery: a sentinel older than its CSV means still writing" \
  crates/geode-data/src/source/discovery.rs \
  '    if sentinel_mtime < mtime {' \
  '    if false {'

run_mutation "discovery: no sentinel means pending, not ready" \
  crates/geode-data/src/source/discovery.rs \
  '    let Ok(sentinel_meta) = std::fs::metadata(sentinel_path) else {' \
  '    let Ok(sentinel_meta) = std::fs::metadata(csv_path) else {'

run_mutation "discovery: waiting too long is reported, not waited on forever" \
  crates/geode-data/src/source/discovery.rs \
  '        return Ok(if waited > spec.pending_timeout {' \
  '        return Ok(if false {'

run_mutation "discovery: an unimplemented readiness strategy is surfaced" \
  crates/geode-data/src/source/discovery.rs \
  '    if let Readiness::StableMtime { polls } = spec.readiness {' \
  '    if let Readiness::StableMtime { polls } = Readiness::Sentinel {'

run_mutation "discovery: an unparsable sentinel is orphaned, not merely pending" \
  crates/geode-data/src/source/discovery.rs \
  '    let sentinel = match parse_sentinel(&text) {
        Ok(s) => s,
        Err(e) => {
            return Ok(CandidateState::Orphaned {
                reason: e.to_string(),
            });
        }
    };' \
  '    let sentinel = match parse_sentinel(&text) {
        Ok(s) => s,
        Err(e) => {
            let _ = e;
            return Ok(CandidateState::Pending);
        }
    };'

run_mutation "discovery: a changed file is reloaded" \
  crates/geode-data/src/source/discovery.rs \
  '        && prev.size == meta.len()
        && prev.source_time == sentinel.as_of' \
  '        && true'

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

run_mutation "as-of: source-time tie breaks on gen_id" \
  crates/geode-data/src/query/as_of.rs \
  'order by source_time desc, gen_id desc' \
  'order by source_time desc'

# Retention deletes. A wrong query shows a wrong number and can be
# re-run; a wrong sweep destroys history that no longer exists to be
# re-read. These entries are here because this file had exactly one, and
# it is the least recoverable code in the data layer.

run_mutation "retention: an empty policy evicts nothing" \
  crates/geode-data/src/store/retention.rs \
  '        if !policy.is_empty() {' \
  '        if true {'

run_mutation "retention: the bookless partition is matchable by its keys" \
  crates/geode-data/src/store/retention.rs \
  '                       and k.book is not distinct from a.book' \
  '                       and k.book = a.book'

run_mutation "retention: age keeps the recent, not the ancient" \
  crates/geode-data/src/store/retention.rs \
  '                    "source_time >= '"'"'{}'"'"'::timestamptz",' \
  '                    "source_time <= '"'"'{}'"'"'::timestamptz",'

run_mutation "retention: every configured rule must be satisfied" \
  crates/geode-data/src/store/retention.rs \
  'keep = keep.join(" and "),' \
  'keep = keep.join(" or "),'

run_mutation "retention: the generation count bound is what it says" \
  crates/geode-data/src/store/retention.rs \
  'keep.push(format!("rn <= {n}"));' \
  'keep.push(format!("rn <= {}", n + 1));'

run_mutation "retention: the remaining bound is the oldest, not the newest" \
  crates/geode-data/src/store/retention.rs \
  '            (Some(a), Some(b)) => Some(a.min(b)),' \
  '            (Some(a), Some(b)) => Some(a.max(b)),'

run_mutation "retention: eviction is counted from the rows actually removed" \
  crates/geode-data/src/store/retention.rs \
  'report.evicted_rows += (before - after).max(0) as usize;' \
  'report.evicted_rows += 0;'

run_mutation "retention: source-time tie breaks on gen_id" \
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

run_mutation "provenance: a join is labelled with its own instant" \
  crates/geode-data/src/query/compile.rs \
  '                    resolved_as_of.insert(join.dataset.clone(), oldest);' \
  '                    let _ = oldest;'

run_mutation "provenance: stalest partition, not newest" \
  crates/geode-data/src/query/compile.rs \
  'if let Some(oldest) = gens.iter().map(|g| g.source_time).min() {' \
  'if let Some(oldest) = gens.iter().map(|g| g.source_time).max() {'

run_mutation "enum: a stale value degrades rather than failing the query" \
  crates/geode-data/src/query/compile.rs \
  'selects.push(format!("try_cast(s.\"{g}\" as {ty}) as \"{g}\""));' \
  'selects.push(format!("s.\"{g}\"::{ty} as \"{g}\""));'

run_mutation "scope: an ordering comparison on a derived dimension is caught at entry" \
  crates/geode-core/src/scope/mod.rs \
  'if dims.get(column).is_some() && !matches!(op, CompareOp::Eq | CompareOp::Ne) {' \
  'if false {' \
  geode-core

run_mutation "scope: a contradiction still names its dimension" \
  crates/geode-core/src/scope/mod.rs \
  'dimensions.retain(|d| !d.values.is_empty() || contradicted.contains(&d.column));' \
  'dimensions.retain(|d| !d.values.is_empty());' \
  geode-core

# ---- derived column attribution (spec §6.3)

run_mutation "derived: a derived column inherits its inputs' attribution" \
  crates/geode-data/src/query/compile.rs \
  '            let referenced = referenced_columns(sql, &columns);' \
  '            let referenced: Vec<&CompiledColumn> = Vec::new();'

run_mutation "derived: attribution meet takes the weaker claim" \
  crates/geode-core/src/attribution.rs \
  '            (NonAttributable, _) | (_, NonAttributable) => NonAttributable,' \
  '            (NonAttributable, _) | (_, NonAttributable) => Additive,' \
  geode-core

run_mutation "derived: a name inside a string literal is not a reference" \
  crates/geode-data/src/query/compile.rs \
  '        if in_string {
            continue;
        }' \
  '        if false {
            continue;
        }'

# ---- config validation (spec §10.1, §6.8)

run_mutation "validation: views are checked when the service opens" \
  crates/geode-data/src/service.rs \
  '            .flat_map(|v| v.validate(&config.schema, &config.dimensions))' \
  '            .flat_map(|_v| Vec::<Diagnostic>::new())'

run_mutation "validation: a scope column is checked against the dataset" \
  crates/geode-core/src/scope/mod.rs \
  '                None if ds.column(&c).is_none() => {' \
  '                None if false => {' \
  geode-core

run_mutation "validation: a derived dimension is not an unknown grouping" \
  crates/geode-core/src/view.rs \
  '            if let Some(d) = dims.get(g) {' \
  '            if let Some(d) = None::<&crate::dimensions::DerivedDimension> {' \
  geode-core

run_mutation "validation: a derived dimension shadowing a column is reported" \
  crates/geode-core/src/view.rs \
  '                if ds.column(g).is_some() {' \
  '                if false {' \
  geode-core

# ---- findings from the phase-2b/prerequisites review round

run_mutation "derived: the value is blanked, not just the marker" \
  crates/geode-data/src/query/compile.rs \
  '            let expr = if blank.is_empty() {' \
  '            let expr = if true {'

run_mutation "derived: comments are stripped before scanning for columns" \
  crates/geode-data/src/query/compile.rs \
  '    let stripped = strip_sql_comments(sql);' \
  '    let stripped = sql.to_string();'

run_mutation "order: tie-breakers use the spine, not the ENUM-cast alias" \
  crates/geode-data/src/query/compile.rs \
  '            order_keys.push(format!("s.\"{g}\" asc"));' \
  '            order_keys.push(format!("\"{g}\" asc"));'

run_mutation "validation: a derived dimension is a legal view column" \
  crates/geode-core/src/view.rs \
  '                    if let Some(d) = dims.get(name) {' \
  '                    if let Some(d) = None::<&crate::dimensions::DerivedDimension> {' \
  geode-core

run_mutation "scope: a contradiction survives further composition" \
  crates/geode-core/src/scope/mod.rs \
  '        let mut contradicted: Vec<String> = if self.impossible {' \
  '        let mut contradicted: Vec<String> = if false {' \
  geode-core

run_mutation "snapshot: dimension codes report a null row as null" \
  crates/geode-core/src/snapshot.rs \
  '        if self.is_null(row) {
            return None;
        }' \
  '        if false {
            return None;
        }' \
  geode-core

run_mutation "snapshot: dimensions read at UInt32 key width" \
  crates/geode-core/src/snapshot.rs \
  '    let d = arr.as_any().downcast_ref::<DictionaryArray<UInt32Type>>()?;' \
  '    let d = None::<&DictionaryArray<UInt32Type>>?;' \
  geode-core

run_mutation "snapshot: a summed i64 measure is readable" \
  crates/geode-core/src/snapshot.rs \
  '    if let Some(values) = arr.as_any().downcast_ref::<Decimal128Array>() {' \
  '    if let Some(values) = None::<&Decimal128Array> {' \
  geode-core

run_mutation "pool: shutdown does not deliver its own interrupt" \
  crates/geode-data/src/query/pool.rs \
  'if stale || cancelled || q.shutdown {' \
  'if stale || cancelled {'

run_mutation "catalog: the migration clears a crashed load's orphan id" \
  crates/geode-data/src/store/catalog.rs \
  'let start = if latest == 0 { 1 } else { latest + 2 };' \
  'let start = if latest == 0 { 1 } else { latest + 1 };'

run_mutation "catalog: the bookless partition rolls up into the unscoped as-of" \
  crates/geode-data/src/store/catalog.rs \
  '            .filter(|(b, _)| books.is_empty() || b.as_ref().is_some_and(|b| books.contains(b)))' \
  '            .filter(|(b, _)| books.is_empty() || b.as_ref().is_none_or(|b| books.contains(b)))'

run_mutation "catalog: the backfill guard is scoped to its dataset (named book)" \
  crates/geode-data/src/store/catalog.rs \
  'where fg.dataset = ? and fg.batch = ? and fb.book = ?' \
  'where ? is not null and fg.batch = ? and fb.book = ?'

run_mutation "catalog: the backfill guard is scoped to its dataset (bookless)" \
  crates/geode-data/src/store/catalog.rs \
  'where fg.dataset = ? and fg.batch = ? and fb.book is null' \
  'where ? is not null and fg.batch = ? and fb.book is null'

run_mutation "catalog: a generation that never went live is not fresh" \
  crates/geode-data/src/store/catalog.rs \
  '                         and coalesce(fg.archived_only, false) = false' \
  '                         and true'

run_mutation "ingest: the publish event names the partitions written" \
  crates/geode-data/src/ingest/runner.rs \
  'books: loaded.partitions.clone(),' \
  'books: Vec::new(),'

run_mutation "ingest: an archived-only load is recorded as such" \
  crates/geode-data/src/ingest/load.rs \
  '    let archived_only = !published.is_empty()' \
  '    let archived_only = false && !published.is_empty()'

run_mutation "catalog: the archived_only column is added to old catalogs" \
  crates/geode-data/src/store/catalog.rs \
  'ALTER TABLE file_generations ADD COLUMN IF NOT EXISTS archived_only BOOLEAN;' \
  '-- migration removed'

# ---- the bookless partition in the catalog (spec §4.5)

run_mutation "catalog: the bookless partition gets a file_books row" \
  crates/geode-data/src/store/catalog.rs \
  '        for book in &rec.books {' \
  '        for book in rec.books.iter().filter(|b| b.is_some()) {'

run_mutation "catalog: freshness can be asked about a null book" \
  crates/geode-data/src/store/catalog.rs \
  'and fg.batch = ? and fb.book is null' \
  'and fg.batch = ? and fb.book is not null'

run_mutation "ingest: the backfill guard covers every partition written" \
  crates/geode-data/src/ingest/load.rs \
  '    for partition in &partitions {' \
  '    for partition in partitions.iter().filter(|p| p.book.is_some()) {'

# ---- generation id allocation (spec §4.3)

run_mutation "catalog: a gen_id is reserved, not peeked" \
  crates/geode-data/src/store/catalog.rs \
  "        let sql = \"select nextval('file_generations_gen_id')\";" \
  '        let sql = "select coalesce(max(gen_id), 0) + 1 from file_generations";'

run_mutation "catalog: the gen_id sequence starts above existing generations" \
  crates/geode-data/src/store/catalog.rs \
  'let start = if latest == 0 { 1 } else { latest + 2 };' \
  'let start = 1;'

# ---- the grain vocabulary (spec §3.3, §6.3)

run_mutation "vocabulary: pair grain does not carry the underlying dimension" \
  crates/geode-core/src/schema/grain.rs \
  'Grain::UnderlyingPair => &K_INSTRUMENT,' \
  'Grain::UnderlyingPair => &K_PAIR,'

run_mutation "attribution: a derived dimension resolves to its base column" \
  crates/geode-core/src/attribution.rs \
  '        .map(|c| dims.base_column(c.as_str()))' \
  '        .map(|c| c.as_str())'

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

# ---- the query pool (spec §6.7, §7.3, §10.1)

run_mutation "pool: a panicking query does not wedge its view" \
  crates/geode-data/src/query/pool.rs \
  '            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&conn, &req))) {
                Ok(r) => r.map_err(|e| e.to_string()),
                Err(payload) => Err(panic_message(&*payload)),
            };' \
  '            run(&conn, &req).map_err(|e| e.to_string());'

run_mutation "pool: a post-shutdown submit is not queued" \
  crates/geode-data/src/query/pool.rs \
  '        if q.shutdown {
            return id;
        }' \
  '        if false {
            return id;
        }'

run_mutation "pool: a cancelled query delivers nothing, not an error" \
  crates/geode-data/src/query/pool.rs \
  'let cancelled = q.cancelled.remove(&id);' \
  'let cancelled = { q.cancelled.remove(&id); false };'

# No entry for allocating the query id under the lock, nor for holding the
# lock across the stale check and the send. Both are races: the mutation is
# only observable on an interleaving the test cannot force, so an entry
# would report SURVIVED whether the code is right or wrong. They are
# argued in comments at the site instead.

# ---- row order (spec §6.3; the blotter's flatten walk)

run_mutation "order: emitted even when the view declares no sort" \
  crates/geode-data/src/query/compile.rs \
  'let order = format!(" order by {}", order_keys.join(", "));' \
  'let order = if view.sort.is_empty() { String::new() } else { format!(" order by {}", order_keys.join(", ")) };'

run_mutation "order: shallowest first" \
  crates/geode-data/src/query/compile.rs \
  'let mut order_keys = vec!["s.row_depth asc".to_string()];' \
  'let mut order_keys: Vec<String> = Vec::new();'

run_mutation "order: grouping columns break ties" \
  crates/geode-data/src/query/compile.rs \
  '    for g in view.grouping.iter().take(depth) {' \
  '    for g in view.grouping.iter().take(0) {'

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

run_mutation "snapshot: depth reads at DuckDB's own integer width" \
  crates/geode-core/src/snapshot.rs \
  '    read_at_width!(Int64Type);
    read_at_width!(Int32Type);' \
  '    read_at_width!(Int64Type);' \
  geode-core

run_mutation "snapshot: depth reads at DuckDB's own width, end to end" \
  crates/geode-core/src/snapshot.rs \
  '        let depth = self.i64_at(self.depth_col?, row)?;' \
  '        let depth = *self.i64_column("row_depth")?.get(row)?;'

run_mutation "snapshot: a rolled-up dimension cell is null" \
  crates/geode-core/src/snapshot.rs \
  'if row >= d.len() || d.is_null(row) {' \
  'if row >= d.len() {' \
  geode-core

run_mutation "snapshot: dimension cells read at UInt16 key width" \
  crates/geode-core/src/snapshot.rs \
  '    if let Some(d) = arr.as_any().downcast_ref::<DictionaryArray<UInt16Type>>() {
        return dictionary_cell(d, row);
    }' \
  '    if let Some(d) = None::<&DictionaryArray<UInt16Type>> {
        return dictionary_cell(d, row);
    }' \
  geode-core

run_mutation "snapshot: dictionary columns expose UInt16 codes" \
  crates/geode-core/src/snapshot.rs \
  '        return Some((DictCodes::U16(d.keys().values(), d.nulls()), values));' \
  '        return None;' \
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
  '    dict_cell_in(arr, row).or_else(|| str_in(arr, row))' \
  '    str_in(arr, row)' \
  geode-core

# ---- query pool (spec §2.4, §5.1)

run_mutation "pool: coalescing is keyed on the tile, not the view" \
  crates/geode-data/src/query/pool.rs \
  '        q.pending.insert(req.key, (id, req));' \
  '        let key = QueryKey(0); q.pending.insert(key, (id, req));' \
  geode-data \
  two_keys_on_one_view_do_not_coalesce

run_mutation "pool: the tag is echoed, not regenerated" \
  crates/geode-data/src/query/pool.rs \
  '            tag: req.tag,' \
  '            tag: 0,' \
  geode-data \
  the_tag_and_submission_time_are_echoed

# ---- service (spec §5.1)

run_mutation "service: an outcome carries the caller's key" \
  crates/geode-data/src/service.rs \
  '                    key: r.key,' \
  '                    key: QueryKey(0),' \
  geode-data \
  an_outcome_is_addressed_to_the_key_that_asked

run_mutation "service: a grouping override is applied" \
  crates/geode-data/src/service.rs \
  '                regrouped = ViewSpec {
                    grouping: grouping.clone(),
                    ..spec.clone()
                };' \
  '                regrouped = spec.clone();' \
  geode-data \
  a_grouping_override_regroups_the_named_view

run_mutation "service: replace_views actually replaces" \
  crates/geode-data/src/service.rs \
  '        self.config.views = views;' \
  '        let _ = views;' \
  geode-data \
  replacing_views_makes_a_new_view_queryable_and_reports_a_bad_one

# ---- ingest runner (Phase 3 §2.5)

run_mutation "runner: an undeclared dataset is a named failure, not a skip" \
  crates/geode-data/src/ingest/runner.rs \
  '            if !sink(IngestEvent::Failed {
                dataset: item.dataset.clone(),
                batch: item.batch.clone(),
                reason: format!("dataset '"'"'{}'"'"' is not declared", item.dataset),
            }) {
                return;
            }
            continue;' \
  '            continue;' \
  geode-data \
  an_item_naming_an_undeclared_dataset_fails_by_name_and_the_runner_continues

# ---- sources config (Phase 3 §5.2)

run_mutation "sources: an undeclared dataset skips the source" \
  crates/geode-data/src/source/config.rs \
  '                Some(d) if schema.dataset(d).is_some() => d.to_string(),' \
  '                Some(d) => d.to_string(),' \
  geode-data \
  a_missing_or_unknown_dataset_is_an_error_and_the_source_is_skipped

run_mutation "sources: a pattern without a batch capture is dropped" \
  crates/geode-data/src/source/config.rs \
  '                    Ok(re) if re.capture_names().any(|c| c == Some("batch")) => Some(p.to_string()),' \
  '                    Ok(re) if re.capture_names().count() > 0 => Some(p.to_string()),' \
  geode-data \
  a_pattern_without_a_batch_capture_is_dropped_with_a_warning

# ---- discovery scheduler (Phase 3 §5.3)

run_mutation "scheduler: every source is polled immediately at start" \
  crates/geode-data/src/ingest/scheduler.rs \
  '.map(|i| (Instant::now(), i))' \
  '.map(|i| (Instant::now() + std::time::Duration::from_secs(15), i))' \
  geode-data \
  a_file_that_appears_after_start_is_discovered_and_published

run_mutation "scheduler: the poll re-arms" \
  crates/geode-data/src/ingest/scheduler.rs \
  '        due[0] = (Instant::now() + spec.poll_interval, i);' \
  '        due[0] = (Instant::now() + std::time::Duration::from_secs(70), i);' \
  geode-data \
  an_unchanged_directory_submits_nothing_on_later_polls

run_mutation "scheduler: ready files reach the runner" \
  crates/geode-data/src/ingest/scheduler.rs \
  '                if ready > 0 {
                    ingest.submit(plan);
                }' \
  '                let _ = plan;' \
  geode-data \
  a_file_that_appears_after_start_is_discovered_and_published

run_mutation "scheduler: pending-too-long surfaces as health" \
  crates/geode-data/src/ingest/scheduler.rs \
  '            CandidateState::PendingTooLong => Health::PendingTooLong,' \
  '            CandidateState::PendingTooLong => continue,' \
  geode-data \
  a_csv_pending_past_its_timeout_is_a_health_event

run_mutation "service: a publish becomes a Published event" \
  crates/geode-data/src/service.rs \
  '                } => sink(DataEvent::Published {
                    dataset,
                    batch,
                    gen_id,
                    books,
                }),' \
  '                } => {
                    let _ = (dataset, batch, gen_id, books);
                    true
                }' \
  geode-data \
  a_configured_source_is_discovered_loaded_and_announced

# ---- data handle (Phase 3 §5.1)

run_mutation "handle: a refused request is counted" \
  crates/geode-data/src/handle.rs \
  '                    self.dropped.fetch_add(1, Ordering::Relaxed);' \
  '                    let _ = Ordering::Relaxed;' \
  geode-data \
  a_full_channel_refuses_and_counts_rather_than_blocking

run_mutation "handle: a compile failure is delivered as the key's outcome" \
  crates/geode-data/src/handle.rs \
  '                if let Err(e) = service.query(&params) {' \
  '                if let Err(e) = service.query(&params) && false {' \
  geode-data \
  the_real_service_answers_through_the_sink_and_reports_open_failures

# ---- snapshot index accessors (Phase 3 §5.5)

run_mutation "snapshot: column_index resolves the column it names" \
  crates/geode-core/src/snapshot.rs \
  '        self.meta.iter().position(|m| m.name == name)' \
  '        self.meta.iter().position(|m| m.name == name).map(|i| i.saturating_sub(1))' \
  geode-core \
  index_accessors_agree_with_their_by_name_twins_under_every_type

run_mutation "snapshot: column_at bounds-checks the index" \
  crates/geode-core/src/snapshot.rs \
  '        (idx < batch.num_columns()).then(|| batch.column(idx).as_ref())' \
  '        Some(batch.column(idx.min(batch.num_columns() - 1)).as_ref())' \
  geode-core \
  index_accessors_agree_with_their_by_name_twins_under_every_type

run_mutation "snapshot: misaligned meta is refused" \
  crates/geode-core/src/snapshot.rs \
  '            if names != described {' \
  '            if false && names != described {' \
  geode-core \
  a_meta_list_that_disagrees_with_the_batch_is_refused

# ---- tree index (Phase 3 §5.5)
#
# Two entries the plan proposed are not here, and the reason is the one
# this file's header warns about: an entry can name a defence that no
# fixture can reach.
#
#   "a parent is found by prefix, not by position" (`if prefix_eq(...)`
#   -> `if true`) SURVIVED. `prefix_eq` runs only on a candidate the hash
#   table already handed back, so it is purely a collision guard: with
#   distinct FNV-1a hashes for distinct prefixes — which every fixture
#   has — the first candidate is always the right parent and skipping the
#   check changes nothing. Catching it would need an engineered 64-bit
#   collision. The claim in its name is pinned below instead, at a point
#   the fixtures do reach.
#
#   "NULL is not the empty string" (`feed(0x00)` -> `feed(0x01)`)
#   SURVIVED, and so did the fallback the plan offered for it
#   (`None => true` -> `None => false` in `prefix_eq`, whose absent-column
#   arm no fixture evaluates: the one fixture with an absent grouping
#   column has rows only at depths 0 and 1, so every `prefix_eq` call
#   there is `take(0)` and `all()` returns true without reading an arm).
#   NULL-vs-"" is defended twice — the hash token and `prefix_eq`'s
#   `Option<&str>` comparison — and each defence rescues a mutation of the
#   other, which is this file's "two defences overlapping so neither is
#   isolated". No single-line mutation isolates it. The other half of the
#   unplaced contract is pinned instead.

run_mutation "tree: a child attaches to the row its prefix names, not to a neighbour" \
  crates/geode-core/src/tree.rs \
  '                        Some(p) => parent[row] = p,' \
  '                        Some(_) => parent[row] = *by_depth[d - 1].last().unwrap(),' \
  geode-core \
  children_are_found_by_prefix_not_by_contiguity

run_mutation "tree: an unplaced row is counted" \
  crates/geode-core/src/tree.rs \
  '                            unplaced += 1;' \
  '                            unplaced += 0;' \
  geode-core \
  a_row_whose_parent_is_missing_attaches_to_the_root_and_is_counted

run_mutation "tree: an unplaced row still appears under the root" \
  crates/geode-core/src/tree.rs \
  '                            parent[row] = roots.first().copied().unwrap_or(NO_PARENT);' \
  '                            parent[row] = NO_PARENT;' \
  geode-core \
  a_row_whose_parent_is_missing_attaches_to_the_root_and_is_counted

run_mutation "tree: children keep row order" \
  crates/geode-core/src/tree.rs \
  '        for (r, &p) in parent.iter().enumerate() {' \
  '        for (r, &p) in parent.iter().enumerate().rev() {' \
  geode-core \
  children_keep_row_order_so_a_declared_sort_is_the_default_sibling_order

# ---- column presentation (Phase 3 §6.2)

run_mutation "view: a format override applies over the kind default" \
  crates/geode-core/src/view.rs \
  '            precision: p.precision.unwrap_or(self.precision),' \
  '            precision: self.precision,' \
  geode-core \
  presentation_is_parsed_per_column_and_defaults_are_per_kind

if [[ -n "$changed_ref" ]]; then
  echo "skipped $skipped entries whose files are unchanged since $changed_ref"
fi
