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
  # geode-app is bin-only (no [lib] target — see its Cargo.toml), so
  # `--lib` fails outright with "no library targets found"; `--bins`
  # is the equivalent for it. Every other package here is lib-only, so
  # `--lib` stays the default.
  local target_flag="--lib"
  if [[ "$pkg" == "geode-app" ]]; then
    target_flag="--bins"
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
    if cargo test -p "$pkg" $target_flag -- "$filter" >"$log" 2>&1; then
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
    if cargo test -p "$pkg" $target_flag >"$log" 2>&1; then
      echo "SURVIVED  $name  <-- no test sees this"
    else
      echo "caught*   $name  <-- caught by a test other than '$filter'"
    fi
  else
    if cargo test -p "$pkg" $target_flag >"$log" 2>&1; then
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

run_mutation "views: a config_version header is not a spurious diagnostic" \
  crates/geode-core/src/view.rs \
  '            if name == "config_version" {
                continue;
            }' \
  '            if false {
                continue;
            }' \
  geode-core \
  a_view_config_version_header_is_not_a_spurious_diagnostic

run_mutation "dimensions: a config_version header is not a spurious diagnostic" \
  crates/geode-core/src/dimensions.rs \
  '            if name == "config_version" {
                continue;
            }' \
  '            if false {
                continue;
            }' \
  geode-core \
  a_dimension_config_version_header_is_not_a_spurious_diagnostic

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

# "probe: a blanked cell renders blank, not 0.00" retired (Phase 3c
# Task 9): the throwaway diagnostic tile it anchored on is deleted
# entirely (spec §9 step 5). The behaviour it defended — a NULL measure
# reads as blank, never 0.00 — lives on as the blotter's own read path
# and is covered there by "cache: a NULL measure is None, never a
# number" a few entries below (`crates/geode-blotter/src/core/cache.rs`).

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

# ---- keymap counts (Phase 3 §3.3)

run_mutation "matcher: digits count only under a counting context" \
  crates/geode-shell/src/keymap/matcher.rs \
  '            && stack.last().is_some_and(|c| c.has_flag(COUNTS))' \
  '            && true' \
  geode-shell \
  digits_are_ordinary_keys_outside_a_counting_context

run_mutation "matcher: a leading zero is a key" \
  crates/geode-shell/src/keymap/matcher.rs \
  '            && (digit != 0 || self.count.is_some())' \
  '            && true' \
  geode-shell \
  a_leading_zero_is_a_key_and_a_later_zero_is_a_digit

run_mutation "matcher: a dead end clears the count" \
  crates/geode-shell/src/keymap/matcher.rs \
  '        self.pending.clear();
        self.count = None;
        MatchResult::NoMatch' \
  '        self.pending.clear();
        MatchResult::NoMatch' \
  geode-shell \
  a_count_survives_a_pending_sequence_and_dies_with_a_dead_end

run_mutation "matcher: the count is capped" \
  crates/geode-shell/src/keymap/matcher.rs \
  '                    .min(MAX_COUNT),' \
  '                    ,' \
  geode-shell \
  the_count_is_capped

# ---- grouping slots and the frame (Phase 3 §4)

run_mutation "groupings: an unknown column drops the slot" \
  crates/geode-core/src/groupings.rs \
  '            if let Some(unknown) = grouping.iter().find(|c| !known(c)) {' \
  '            if let Some(unknown) = grouping.iter().find(|c| !known(c) && false) {' \
  geode-core \
  an_unknown_column_is_an_error_for_that_slot_only

run_mutation "frame: an empty slot cannot be activated" \
  crates/geode-shell/src/frame.rs \
  '            && self.slots.get(n).is_none()' \
  '            && false' \
  geode-shell \
  an_empty_slot_cannot_be_activated

run_mutation "frame: set_scope bumps only the scope counter" \
  crates/geode-shell/src/frame.rs \
  '        self.previous_scope = Some(std::mem::replace(&mut self.scope, scope));
        self.versions.scope += 1;' \
  '        self.previous_scope = Some(std::mem::replace(&mut self.scope, scope));
        self.versions.scope += 1;
        self.versions.grouping += 1;' \
  geode-shell \
  each_mutation_bumps_exactly_its_own_counter

run_mutation "frame: a vanished active slot is cleared on reload" \
  crates/geode-shell/src/frame.rs \
  '        if self
            .active_slot
            .is_some_and(|n| self.slots.get(n).is_none())
        {' \
  '        if false {' \
  geode-shell \
  replacing_slots_bumps_config_and_grouping_and_drops_a_vanished_active_slot

# ---- module hosting (Phase 3 §3)

run_mutation "hosting: an unknown action reaches the focused occupant with its count" \
  crates/geode-shell/src/shell/input.rs \
  '                o.content.dispatch(action, count, window, cx);' \
  '                o.content.dispatch(action, None, window, cx);' \
  geode-shell \
  a_key_in_the_occupants_context_reaches_its_dispatch_with_the_count

run_mutation "hosting: a closed tile drops its occupant" \
  crates/geode-shell/src/shell/occupants.rs \
  '        self.occupants.retain(|id, _| all.contains(id));' \
  '        let _ = &all;' \
  geode-shell \
  closing_a_tile_drops_its_occupant_and_switching_workspaces_toggles_visibility

run_mutation "hosting: leaving the screen is announced" \
  crates/geode-shell/src/shell/occupants.rs \
  '                o.content.set_visible(false, cx);' \
  '                let _ = o;' \
  geode-shell \
  closing_a_tile_drops_its_occupant_and_switching_workspaces_toggles_visibility

run_mutation "hosting: a fallback factory never sees a mismatched record's state" \
  crates/geode-shell/src/shell/occupants.rs \
  '            let state = matched.and(restored.as_ref()).map(|r| &r.state);' \
  '            let state = restored.as_ref().map(|r| &r.state);' \
  geode-shell \
  a_restored_tile_of_an_unknown_kind_falls_back_without_its_state

run_mutation "hosting: an occupant created outside the active set is told it is hidden (I2, final review)" \
  crates/geode-shell/src/shell/occupants.rs \
  '            occupant.content.set_visible(active.contains(id), cx);' \
  '            occupant.content.set_visible(true, cx);' \
  geode-shell \
  an_occupant_created_outside_the_active_workspace_is_told_it_is_hidden

# ---- session tiles (Phase 3 §3.5)

run_mutation "session: a record for a tile not in the layout is dropped" \
  crates/geode-shell/src/session.rs \
  '                    if !here.contains(&id) {' \
  '                    if false {' \
  geode-shell \
  a_tile_record_for_an_id_not_in_that_workspace_is_dropped_with_a_warning

run_mutation "session: tile state round-trips" \
  crates/geode-shell/src/session.rs \
  '            if !record.state.is_empty() {
                t.insert(
                    "state".to_string(),
                    toml::Value::Table(record.state.clone()),
                );
            }' \
  '' \
  geode-shell \
  tiles_round_trip_with_their_kind_and_opaque_state

run_mutation "session: a state-only change alone still flushes" \
  crates/geode-shell/src/shell/session_io.rs \
  '        if !self.session_dirty && tiles == self.last_tiles_written {' \
  '        if !self.session_dirty {' \
  geode-shell \
  a_module_state_change_alone_flushes_once_with_the_new_state

# ---- command line (Phase 3 §3.4)

run_mutation "commandline: an ambiguous word is refused, never guessed" \
  crates/geode-shell/src/commandline.rs \
  '    if candidates.len() == 1 {' \
  '    if !candidates.is_empty() {' \
  geode-shell \
  submit_runs_accepts_or_refuses

run_mutation "commandline: an exact word runs as typed" \
  crates/geode-shell/src/commandline.rs \
  '    if typed.is_empty() || candidates.is_empty() || words.iter().any(|w| w == typed) {' \
  '    if typed.is_empty() || candidates.is_empty() {' \
  geode-shell \
  submit_runs_accepts_or_refuses

run_mutation "commandline: escape on a find is a cancel" \
  crates/geode-shell/src/shell/commandline_ctl.rs \
  '            o.content.find(FindEvent::Cancelled, window, cx);' \
  '            let _ = o;' \
  geode-shell \
  slash_streams_find_events_and_escape_cancels

# "commandline: a tile mouse-down cancels an open line (fix round 1)"
# retired (I1, final review): removing this call is no longer an
# independently observable behaviour. A tile mouse-down always changes
# the active workspace's own focused tile away from `command_line.tile`,
# which the render-time backstop added for I1 (see the two entries just
# below) now also catches — and, checked directly, `run_until_parked`
# after `simulate_mouse_down`/`up` already runs that render before
# control returns to the test, so no assertion (before or after an
# explicit `window.draw`) can tell "the explicit call ran" apart from
# "the backstop compensated in the same pass" any more. Confirmed by
# hand: mutating away *both* this call and the backstop together is what
# it now takes to fail `a_mouse_down_on_another_tile_cancels_an_open_
# command_line` — that pairing is exactly what the two entries below
# already defend. Keeping this one would only ever show `SURVIVED`,
# which would misstate the situation as an untested gap rather than the
# deliberate, now-redundant fast path `render`'s own doc comment
# describes.

run_mutation "commandline: a second tab refreshes the accepted word range instead of corrupting it (C1, final review)" \
  crates/geode-shell/src/shell/commandline_ctl.rs \
  '                        c.word = c.word.start..cursor;' \
  '                        let _ = cursor;' \
  geode-shell \
  a_second_tab_cycles_the_completion_instead_of_corrupting_the_line

run_mutation "commandline: switching workspaces cancels an open line (I1, final review)" \
  crates/geode-shell/src/shell/render.rs \
  '            self.services.workspaces.active().focused_tile() != Some(line.tile)' \
  '            false' \
  geode-shell \
  switching_workspaces_cancels_an_open_command_line

run_mutation "commandline: losing keyboard focus to another surface cancels an open line (I1, final review)" \
  crates/geode-shell/src/shell/render.rs \
  '                || !self
                    .command_input
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)' \
  '                || false' \
  geode-shell \
  clicking_the_filter_input_cancels_an_open_command_line

# ---- frame keys, the readout, and config reload (Phase 3 §4.2, §4.5)

run_mutation "frame: a sources change is a restart, not a silent apply" \
  crates/geode-shell/src/shell/hot_reload.rs \
  '                ("sources", &self.sources_baseline),
                ("datasets", &self.datasets_baseline),' \
  '                ("nonesuch", &self.sources_baseline),
                ("nonesuch", &self.datasets_baseline),' \
  geode-shell \
  a_reloaded_groupings_doc_replaces_the_slots_and_a_sources_change_asks_for_a_restart

run_mutation "frame: a groupings/datasets/dimensions change replaces the frame's slots (fix round 1)" \
  crates/geode-shell/src/shell/hot_reload.rs \
  '            if groupings_changed {' \
  '            if false {' \
  geode-shell \
  a_reloaded_groupings_doc_replaces_the_slots_and_a_sources_change_asks_for_a_restart

run_mutation "frame: a views/dimensions change reaches the frame and emits ConfigReloaded (fix round 1)" \
  crates/geode-shell/src/shell/hot_reload.rs \
  '            if views_changed {' \
  '            if false {' \
  geode-shell \
  a_reloaded_groupings_doc_replaces_the_slots_and_a_sources_change_asks_for_a_restart

# ---- Phase 3c Task 0 (deferred 3b cleanups: M4, M8, M9, slot-rebuild DRY)

run_mutation "hot_reload: rebuild_slots is the one place both ShellView::new and apply_reload build GroupingSlots (DRY)" \
  crates/geode-shell/src/shell/hot_reload.rs \
  '    let (slots, diags) = config
        .doc("groupings")
        .map(|d| GroupingSlots::from_doc(d, &schema, &dims))
        .unwrap_or_default();' \
  '    let (slots, diags) = config
        .doc("nonesuch")
        .map(|d| GroupingSlots::from_doc(d, &schema, &dims))
        .unwrap_or_default();' \
  geode-shell \
  a_reloaded_groupings_doc_replaces_the_slots_and_a_sources_change_asks_for_a_restart

run_mutation "commandline: word_at clamps a non-char-boundary cursor down before slicing (M4)" \
  crates/geode-shell/src/commandline.rs \
  '    while !line.is_char_boundary(cursor) {' \
  '    while false {' \
  geode-shell \
  a_cursor_on_a_non_char_boundary_clamps_down_instead_of_panicking

run_mutation "frame: restart_required clears once sources/datasets match the baseline again (M8)" \
  crates/geode-shell/src/shell/hot_reload.rs \
  '                self.restart_required = None;' \
  '                let _ = &self.restart_required;' \
  geode-shell \
  reverting_a_sources_edit_back_to_the_baseline_clears_restart_required

run_mutation "reload: ConfigReloaded is queued before ANY frame.update, including groupings_changed's (I2, residual fix)" \
  crates/geode-shell/src/shell/hot_reload.rs \
  '            if views_changed {
                cx.emit(ShellEvent::ConfigReloaded);
            }
            if groupings_changed {
                let slots = rebuild_slots(&self.services.config);
                self.frame.update(cx, |f, cx| {
                    if f.replace_slots(slots) {
                        cx.notify();
                    }
                });
            }
            if views_changed {
                self.frame.update(cx, |f, cx| {
                    f.note_config_reloaded();
                    cx.notify();
                });
            }' \
  '            if groupings_changed {
                let slots = rebuild_slots(&self.services.config);
                self.frame.update(cx, |f, cx| {
                    if f.replace_slots(slots) {
                        cx.notify();
                    }
                });
            }
            if views_changed {
                cx.emit(ShellEvent::ConfigReloaded);
                self.frame.update(cx, |f, cx| {
                    f.note_config_reloaded();
                    cx.notify();
                });
            }' \
  geode-shell \
  emits_config_reloaded_before_the_frame_notifies

run_mutation "frame: readout is rebuilt when versions change" \
  crates/geode-shell/src/frame.rs \
  '        if let Some((cached_versions, cached)) = self.readout_cache.borrow().as_ref()
            && *cached_versions == versions
        {
            return Rc::clone(cached);
        }' \
  '        if let Some((cached_versions, cached)) = self.readout_cache.borrow().as_ref()
            && *cached_versions != versions
        {
            return Rc::clone(cached);
        }' \
  geode-shell \
  readout_is_rebuilt_only_when_versions_change

run_mutation "theme: write_atomic's temp name derives from the target file, not a hardcoded app.toml (M9)" \
  crates/geode-shell/src/theme.rs \
  '    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("geode-write");' \
  '    let file_name = "app.toml";' \
  geode-shell \
  the_temp_name_derives_from_the_target_file_not_a_hardcoded_app_toml

# ---- blotter core (Phase 3 §6)

run_mutation "plan: attribution is per depth" \
  crates/geode-blotter/src/core/plan.rs \
  '            .and_then(|c| c.attribution.get(depth).copied())' \
  '            .and_then(|c| c.attribution.first().copied())' \
  geode-blotter \
  attribution_is_per_column_per_depth_and_semi_joined_dimensions_are_named

run_mutation "flatten: only open nodes are descended into" \
  crates/geode-blotter/src/core/flatten.rs \
  '    if expansion.is_open(path) {' \
  '    if true {' \
  geode-blotter \
  opening_a_node_shows_its_children_in_row_order_and_descends_only_into_open_nodes

run_mutation "flatten: NULL sorts last in both directions" \
  crates/geode-blotter/src/core/flatten.rs \
  '                if is_null(snapshot, idx, numeric, a) || is_null(snapshot, idx, numeric, b) =>' \
  '                if false =>' \
  geode-blotter \
  a_sort_orders_siblings_within_their_parent_with_null_last

run_mutation "expansion: the depth bound is one past the deepest open node" \
  crates/geode-blotter/src/core/expansion.rs \
  '        .saturating_add(1)' \
  '        .saturating_add(2)' \
  geode-blotter \
  the_depth_bound_is_one_past_the_deepest_open_node_capped_at_the_grouping

run_mutation "expansion: closing one node under open_all leaves its siblings open" \
  crates/geode-blotter/src/core/expansion.rs \
  '            !self.closed.contains(path)' \
  '            true' \
  geode-blotter \
  close_under_open_all_closes_only_that_node

run_mutation "find: fzf narrows and vim does not" \
  crates/geode-blotter/src/core/find.rs \
  '            FindStyle::Vim => find_match(texts, self.origin, FindDirection::Forward, query),' \
  '            FindStyle::Vim => { self.narrowed = Some(filter_matches(texts, query)); find_match(texts, self.origin, FindDirection::Forward, query) }' \
  geode-blotter \
  vim_style_jumps_as_typed_commits_and_repeats

run_mutation "cursor: a counted G is a row number" \
  crates/geode-blotter/src/core/cursor.rs \
  '                self.row = (c.max(1) as usize - 1).min(len.saturating_sub(1));' \
  '                self.row = len.saturating_sub(1); let _ = c;' \
  geode-blotter \
  row_motion_is_counted_and_clamped

run_mutation "cache: a NULL measure is None, never a number" \
  crates/geode-blotter/src/core/cache.rs \
  '            let value = snapshot.f64_at(idx, row)?;' \
  '            let value = snapshot.f64_at(idx, row).unwrap_or(0.0);' \
  geode-blotter \
  cells_honour_the_read_paths_opinions

run_mutation "cache: a window move keeps overlapping rows" \
  crates/geode-blotter/src/core/cache.rs \
  '            if old.contains(&r) {' \
  '            if false {' \
  geode-blotter \
  a_window_move_refills_only_the_rows_that_entered

run_mutation "format: the sign is of the rounded value" \
  crates/geode-blotter/src/core/format.rs \
  '    let sign = if rounded == 0.0 {' \
  '    let sign = if scaled == 0.0 {' \
  geode-blotter \
  precision_thousands_and_sign

run_mutation "format: scale divides before precision" \
  crates/geode-blotter/src/core/format.rs \
  '        s => value / s.divisor(),' \
  '        s => { let _ = s; value }' \
  geode-blotter \
  parentheses_and_scale

run_mutation "yank: numbers are raw and unscaled" \
  crates/geode-blotter/src/core/yank.rs \
  '                        let _ = write!(s, "{v}");' \
  '                        let _ = write!(s, "{:.2}", v);' \
  geode-blotter \
  tsv_has_a_header_indented_tree_text_raw_numbers_and_blanks

run_mutation "commands: sort desc is parsed" \
  crates/geode-blotter/src/core/commands.rs \
  '                    descending: true,' \
  '                    descending: false,' \
  geode-blotter \
  every_command_parses

run_mutation "commands: a completions cursor mid-character is clamped to a boundary" \
  crates/geode-blotter/src/core/commands.rs \
  '    while !line.is_char_boundary(cursor) {' \
  '    while false {' \
  geode-blotter \
  completions_clamp_a_cursor_inside_a_multibyte_char

run_mutation "delegate: the cursor follows its node across a new snapshot" \
  crates/geode-blotter/src/delegate.rs \
  '            self.cursor.row = restore_by_path(&self.shown, snapshot, plan, &path, self.cursor.row);' \
  '            let _ = restore_by_path(&self.shown, snapshot, plan, &path, self.cursor.row);' \
  geode-blotter \
  applying_a_snapshot_builds_the_plan_flattens_and_keeps_the_cursor_node

run_mutation "delegate: a regroup with a different grouping rebuilds the plan" \
  crates/geode-blotter/src/delegate.rs \
  '            Some(p) => p.grouping != grouping || !p.same_columns(&snapshot),' \
  '            Some(_p) => false,' \
  geode-blotter \
  a_regroup_prunes_expansion_and_rebuilds_the_plan

run_mutation "delegate: narrowed positions actually filter what is shown" \
  crates/geode-blotter/src/delegate.rs \
  '            Some(positions) => self.shown.extend(
                positions
                    .iter()
                    .filter_map(|&i| self.visible.get(i).copied()),
            ),
        }
    }

    pub fn set_narrowed' \
  '            Some(_positions) => self.shown.extend_from_slice(&self.visible),
        }
    }

    pub fn set_narrowed' \
  geode-blotter \
  narrowing_changes_what_is_shown_and_the_cache_window_follows_shown_rows

run_mutation "delegate: narrowed values are positions into visible, not row ids" \
  crates/geode-blotter/src/delegate.rs \
  '            Some(positions) => self.shown.extend(
                positions
                    .iter()
                    .filter_map(|&i| self.visible.get(i).copied()),
            ),
        }
    }

    pub fn set_narrowed' \
  '            Some(positions) => self.shown.extend(
                self.visible
                    .iter()
                    .copied()
                    .filter(|r| positions.contains(&(*r as usize))),
            ),
        }
    }

    pub fn set_narrowed' \
  geode-blotter \
  narrowing_uses_positions_into_visible_not_row_ids_even_when_they_differ

run_mutation "delegate: narrowing invalidates the cache" \
  crates/geode-blotter/src/delegate.rs \
  '        self.cursor.clamp(self.shown.len(), cols);
        self.invalidate_cells();
    }' \
  '        self.cursor.clamp(self.shown.len(), cols);
    }' \
  geode-blotter \
  narrowing_changes_what_is_shown_and_the_cache_window_follows_shown_rows

run_mutation "delegate: apply_snapshot prunes expansion to the new grouping" \
  crates/geode-blotter/src/delegate.rs \
  '        self.expansion.prune_to(grouping.len());' \
  '        let _ = grouping.len();' \
  geode-blotter \
  a_regroup_to_a_shallower_grouping_prunes_a_path_deeper_than_it_can_reach

run_mutation "delegate: apply_snapshot invalidates the cache even without narrowing" \
  crates/geode-blotter/src/delegate.rs \
  '        self.cursor.clamp(self.shown.len(), plan.columns.len());
        self.invalidate_cells();
    }' \
  '        self.cursor.clamp(self.shown.len(), plan.columns.len());
    }' \
  geode-blotter \
  apply_snapshot_invalidates_the_cache_and_refills_it

run_mutation "delegate: invalidate_cells also clears the cached tree glyphs" \
  crates/geode-blotter/src/delegate.rs \
  '        self.cache.invalidate();
        self.glyphs.clear();
        if !w.is_empty() {' \
  '        self.cache.invalidate();
        if !w.is_empty() {' \
  geode-blotter \
  invalidate_cells_clears_stale_glyphs_when_shown_shrinks_past_the_old_window

run_mutation "delegate: invalidate_cells refills the window it had" \
  crates/geode-blotter/src/delegate.rs \
  '        self.glyphs.clear();
        if !w.is_empty() {
            let end = w.end.min(self.shown.len());
            if w.start < end {
                self.refill_window(w.start..end);
            }
        }
    }' \
  '        self.glyphs.clear();
    }' \
  geode-blotter \
  a_regroup_that_keeps_the_window_refills_it_immediately

run_mutation "delegate: any_determined reflects the whole cached window" \
  crates/geode-blotter/src/delegate.rs \
  '        self.cache
            .set_window(window.clone(), cols, |shown_row, col| {
                let row = *shown.get(shown_row)? as usize;
                cell(snapshot, plan, row, col)
            });
        // Scanned over the *whole* current window, not just the rows
        // this call'"'"'s `fill` closure actually ran for: `set_window`
        // keeps overlapping rows without re-invoking `fill`, so a row
        // that entered on an earlier call and stayed cached must still
        // be able to hold the flag up after a scroll that brings in
        // nothing but non-determined rows.
        let determined_window = self.cache.window();
        self.any_determined = determined_window.into_iter().any(|row: usize| {
            (0..cols).any(|col| {
                self.cache
                    .get(row, col)
                    .is_some_and(|c| c.attribution == Attribution::DeterminedNonAdditive)
            })
        });' \
  '        let mut any_determined = false;
        self.cache
            .set_window(window.clone(), cols, |shown_row, col| {
                let row = *shown.get(shown_row)? as usize;
                let c = cell(snapshot, plan, row, col)?;
                if c.attribution == Attribution::DeterminedNonAdditive {
                    any_determined = true;
                }
                Some(c)
            });
        self.any_determined = any_determined;' \
  geode-blotter \
  any_determined_reflects_the_whole_window_not_just_newly_entered_rows

# ---- blotter tile (Phase 3 §6.5, §6.7, §6.8, §3.1, §4.3)

run_mutation "tile: a stale tag is dropped" \
  crates/geode-blotter/src/tile.rs \
  '        if outcome.tag != self.tag {' \
  '        if false {' \
  geode-blotter \
  a_stale_outcome_is_dropped_an_error_keeps_the_last_snapshot_and_timing_is_recorded

run_mutation "tile: a pinned tile ignores the slot" \
  crates/geode-blotter/src/tile.rs \
  '            || (self.pin == Pin::None && acted.grouping != now.grouping)' \
  '            || acted.grouping != now.grouping' \
  geode-blotter \
  a_frame_slot_change_requeries_once_and_a_pinned_tile_ignores_it

run_mutation "tile: the depth bound is requested, not everything" \
  crates/geode-blotter/src/tile.rs \
  '            d.depth_bound(grouping.len()).max(1)' \
  '            usize::MAX' \
  geode-blotter \
  showing_the_tile_submits_one_query_keyed_by_the_tile_with_the_views_grouping

run_mutation "tile: a query error keeps the last snapshot" \
  crates/geode-blotter/src/tile.rs \
  '            Err(e) => self.error = Some(e),' \
  '            Err(e) => { self.error = Some(e); self.table.update(cx, |t, _| *t.delegate_mut() = BlotterDelegate::new()); }' \
  geode-blotter \
  a_stale_outcome_is_dropped_an_error_keeps_the_last_snapshot_and_timing_is_recorded

run_mutation "tile: fzf narrowing matches the un-narrowed list" \
  crates/geode-blotter/src/tile.rs \
  '                    self.table.read(cx).delegate().visible_texts()' \
  '                    self.table.read(cx).delegate().shown_texts()' \
  geode-blotter \
  find_jumps_under_vim_and_narrows_under_fzf

run_mutation "tile: the configured threshold is the one used" \
  crates/geode-blotter/src/tile.rs \
  '                    > self.stale_after.get()' \
  '                    > Duration::from_secs(15 * 60)' \
  geode-blotter \
  a_tiles_stale_threshold_is_the_factorys_configured_value

run_mutation "delegate: move_column refills the window it already had" \
  crates/geode-blotter/src/delegate.rs \
  '        // same tree, and the window is only ever tens of rows.
        self.invalidate_cells();
        cx.notify();
    }' \
  '        // same tree, and the window is only ever tens of rows.
        cx.notify();
    }' \
  geode-blotter \
  move_column_refills_the_window_immediately

# ---- geode-app: the data bridge, the roster, --demo (Phase 3 §5.1, §5.4, §7.1)

run_mutation "bridge: dropped_events counted on a refused try_send" \
  crates/geode-app/src/bridge.rs \
  '            dropped.fetch_add(1, Ordering::Relaxed);
            false' \
  '            false' \
  geode-app \
  a_refused_event_is_counted_as_dropped_rather_than_lost_silently

run_mutation "bridge: db_path precedence — config wins over demo and the platform dir" \
  crates/geode-app/src/bridge.rs \
  '    if let Some(p) = config.get("app", "data.db_path").and_then(|v| v.as_str()) {' \
  '    if false && let Some(p) = config.get("app", "data.db_path").and_then(|v| v.as_str()) {' \
  geode-app \
  the_database_path_prefers_config_then_demo_then_the_platform_dir

run_mutation "demo: the sources doc's paths glob is rewritten onto the emitted directory" \
  crates/geode-app/src/demo.rs \
  '        source_dir.join("*.csv").to_string_lossy()' \
  '        "/nonexistent/*.csv".to_string()' \
  geode-app \
  the_demo_layer_is_complete_and_points_sources_at_the_directory

# Fix round 1, Finding 1: every branch of the drain loop, not just
# `Query`, must reach the shell through `window.update` and end the task
# the first time the window is gone. This entry restores the pre-fix
# shape — only `Query` routes through `window.update`; the dropped-events
# status update, `Published`, and `Health` go back to a bare `cx.update`
# on a standalone `Entity<ShellView>` clone the task keeps alive forever
# — so a `Health` event sent after the window closes no longer ends the
# task, which is exactly what the covering test sends and checks for.
run_mutation "bridge: every event branch, not just Query, ends the drain task on a closed window" \
  crates/geode-app/src/bridge.rs \
  '            let handled = window.update(cx, |root, window, cx| {
                let Ok(shell) = root.view().clone().downcast::<ShellView>() else {
                    return;
                };
                if now_dropped != last_dropped {
                    shell.update(cx, |s, cx| {
                        s.set_data_status(Some(format!("data: {now_dropped} event(s) dropped")), cx)
                    });
                }
                match event {
                    DataEvent::Query(outcome) => {
                        shell.update(cx, |s, cx| s.deliver(outcome, window, cx));
                    }
                    DataEvent::Published {
                        dataset,
                        batch,
                        gen_id,
                        ..
                    } => {
                        eprintln!("[data] published {dataset}/{batch} gen {gen_id}");
                        let frame = shell.read(cx).frame().clone();
                        frame.update(cx, |f, cx| {
                            f.note_published();
                            cx.notify();
                        });
                    }
                    DataEvent::Health {
                        source,
                        worst,
                        detail,
                    } => {
                        eprintln!("[data] health {source}: {} — {detail}", worst.label());
                        shell.update(cx, |s, cx| {
                            s.set_data_status(Some(format!("{source}: {}", worst.label())), cx)
                        });
                    }
                    DataEvent::Diagnostics(diags) => {
                        for d in diags {
                            eprintln!("[data] {d}");
                        }
                    }
                }
            });
            if handled.is_err() {
                return; // the window is gone
            }
            last_dropped = now_dropped;' \
  '            if now_dropped != last_dropped {
                last_dropped = now_dropped;
                cx.update(|cx| {
                    shell.update(cx, |s, cx| {
                        s.set_data_status(Some(format!("data: {now_dropped} event(s) dropped")), cx)
                    });
                });
            }
            let outcome = match event {
                DataEvent::Query(outcome) => Some(outcome),
                DataEvent::Published {
                    dataset,
                    batch,
                    gen_id,
                    ..
                } => {
                    eprintln!("[data] published {dataset}/{batch} gen {gen_id}");
                    cx.update(|cx| {
                        let frame = shell.read(cx).frame().clone();
                        frame.update(cx, |f, cx| {
                            f.note_published();
                            cx.notify();
                        });
                    });
                    None
                }
                DataEvent::Health {
                    source,
                    worst,
                    detail,
                } => {
                    eprintln!("[data] health {source}: {} — {detail}", worst.label());
                    cx.update(|cx| {
                        shell.update(cx, |s, cx| {
                            s.set_data_status(Some(format!("{source}: {}", worst.label())), cx)
                        });
                    });
                    None
                }
                DataEvent::Diagnostics(diags) => {
                    for d in diags {
                        eprintln!("[data] {d}");
                    }
                    None
                }
            };
            if let Some(outcome) = outcome {
                let delivered = window.update(cx, |root, window, cx| {
                    if let Ok(shell) = root.view().clone().downcast::<ShellView>() {
                        shell.update(cx, |s, cx| s.deliver(outcome, window, cx));
                    }
                });
                if delivered.is_err() {
                    return; // the window is gone
                }
            }' \
  geode-app \
  the_drain_task_ends_on_the_first_event_after_the_window_closes

if [[ -n "$changed_ref" ]]; then
  echo "skipped $skipped entries whose files are unchanged since $changed_ref"
fi
