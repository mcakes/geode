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
  'is_unchanged(&prev, meta.len(), sentinel.as_of)' \
  'true' \
  geode-data a_changed_file_is_ready_again

# ---- as-of routing (spec §6.5)

# Re-anchored (generations-table change): `resolve_generations` no longer
# takes a table list -- it reads the `generations` summary by dataset --
# so "resolving across every grain" is now a property of what
# `history_of` names when the summary is built (`rebuild_generations`,
# called from `DataService::open`'s migration and from `publish_file`'s
# and `sweep`'s own maintenance via the tables they're handed directly).
# Dropping the live half here is the same class of omission the old
# entry caught: a partition whose current generation lives only in
# `_live` would vanish from the rebuilt summary.
run_mutation "as-of: multi-grain generation resolution" \
  crates/geode-data/src/store/ddl.rs \
  '                table_name(dataset, g, TableKind::Archive),
                table_name(dataset, g, TableKind::Live),' \
  '                table_name(dataset, g, TableKind::Archive),' \
  geode-data history_of_names_the_archive_and_live_table_of_every_grain

run_mutation "as-of: current generation read from live" \
  crates/geode-data/src/query/scope_sql.rs \
  '"(select * from {} where {p} union all select * from {} where {p})"' \
  '"(select * from {} where {p} union all select * from {} where {p} and false)"' \
  geode-data as_of_after_the_current_generation_reads_the_current_generation

run_mutation "as-of: relation filters the live side too, not just the archive" \
  crates/geode-data/src/query/scope_sql.rs \
  '"(select * from {} where {p} union all select * from {} where {p})"' \
  '"(select * from {} where {p} union all select * from {})"' \
  geode-data an_as_of_query_reads_only_the_archive_even_through_a_semi_join

# Review round 1 finding (Minor-2): the two entries above only ever drop
# the *live* side's filter. Dropping the *archive* side's is a behaviour
# this fix newly created (the archive side used to be filtered by the
# caller) and nothing above catches it — the only other guard,
# an_archive_era_relation_filters_both_sides, is a string assertion, not
# a value.
run_mutation "as-of: relation filters the archive side too" \
  crates/geode-data/src/query/scope_sql.rs \
  '"(select * from {} where {p} union all select * from {} where {p})"' \
  '"(select * from {} union all select * from {} where {p})"' \
  geode-data a_null_book_partition_is_not_dropped_from_history

run_mutation "as-of: probe era" \
  crates/geode-data/src/query/scope_sql.rs \
  'era.relation(&ds.name, probe),' \
  'table_name(&ds.name, probe, TableKind::Live),'

run_mutation "as-of: ENUM cast era guard" \
  crates/geode-data/src/query/compile.rs \
  'if era.kind != TableKind::Live {' \
  'if false {'

# The generation predicate used to be a per-generation OR chain, then a
# gen_id range plus a tuple semi-join (Phase 4a's as-of baseline fix,
# docs/perf.md), and is now a gen_id IN-list plus the same tuple
# semi-join, pushed into both sides of the era relation — the range
# degenerated on a real archive with spread-out generation ids and
# pruned nothing at all (docs/perf.md, "the range prefilter degenerates
# on a real archive"). The two entries the OR-chain form's removal
# replaced ("as-of: NULL book predicate", "as-of: predicate names the
# source time") no longer match live code either.

run_mutation "as-of: generation IN-list excludes the lowest resolved id" \
  crates/geode-data/src/query/as_of.rs \
  '    ids.sort_unstable();
    ids.dedup();' \
  '    ids.sort_unstable();
    ids.dedup();
    ids.remove(0);' \
  geode-data the_predicate_selects_exactly_the_resolved_generations

run_mutation "as-of: tuple loses the source time" \
  crates/geode-data/src/query/as_of.rs \
  '(batch, book, gen_id, source_time) in (select (b, k, g, t)' \
  '(batch, book, gen_id) in (select (b, k, g)' \
  geode-data the_predicate_names_the_source_time_so_a_reused_gen_id_selects_one_generation

run_mutation "as-of: NULL book dropped from the values tuple" \
  crates/geode-data/src/query/as_of.rs \
  'None => "NULL::varchar".to_string(),' \
  'None => "'\'\''".to_string(),' \
  geode-data the_predicate_selects_a_null_book_partition_from_either_side

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

# ---- the generations summary table (docs/perf.md, "the as-of baseline"
# and its follow-up): a small table maintained inside the publish and
# sweep transactions, resolved from directly instead of scanning the
# archive per requery.

run_mutation "generations: the publish insert removed from the normal branch" \
  crates/geode-data/src/store/publish.rs \
  '        staging = req.staging_table,
        summary = generation_summary_insert(req),' \
  '        staging = req.staging_table,
        summary = String::new(),' \
  geode-data a_normal_publish_records_the_generation_in_the_summary

# Review round 1 (MAJ-1): pins the summary insert's *placement*, not just
# its presence -- reordering `{summary}` and `commit;` within the same
# SQL string, alone, is unobservable (`execute_batch` stops at the first
# failing statement regardless of the order of the ones after it, and on
# success both orderings eventually land the same rows), verified by hand
# before adding this. The mutation that actually reproduces the "phantom
# row" the review named runs the summary insert unconditionally, outside
# the transaction's own success/failure, after the main statement --
# leaving a summary row for a generation whose data never committed.
run_mutation "generations: the publish insert survives a rolled-back transaction" \
  crates/geode-data/src/store/publish.rs \
  '    let sql = format!(
        "begin;
         insert into {archive} select * from {live} where {predicate};
         delete from {live} where {predicate};
         insert into {live}
             select *, {gen}, '"'"'{time}'"'"'::timestamptz from {staging};
         {summary}
         commit;",
        gen = req.gen_id,
        time = req.source_time.to_rfc3339(),
        staging = req.staging_table,
        summary = generation_summary_insert(req),
    );
    if let Err(source) = conn.execute_batch(&sql) {
        let _ = conn.execute_batch("rollback;");
        return Err(StoreError::Sql {
            statement: sql,
            source,
        });
    }' \
  '    let sql = format!(
        "begin;
         insert into {archive} select * from {live} where {predicate};
         delete from {live} where {predicate};
         insert into {live}
             select *, {gen}, '"'"'{time}'"'"'::timestamptz from {staging};
         commit;",
        gen = req.gen_id,
        time = req.source_time.to_rfc3339(),
        staging = req.staging_table,
    );
    let result = conn.execute_batch(&sql);
    if result.is_err() {
        let _ = conn.execute_batch("rollback;");
    }
    let _ = conn.execute_batch(&generation_summary_insert(req));
    if let Err(source) = result {
        return Err(StoreError::Sql {
            statement: sql,
            source,
        });
    }' \
  geode-data a_failed_publish_leaves_no_summary_row

run_mutation "generations: the publish insert removed from the archived-only branch" \
  crates/geode-data/src/store/publish.rs \
  '            req.staging_table,
            summary = generation_summary_insert(req),' \
  '            req.staging_table,
            summary = String::new(),' \
  geode-data an_archived_only_publish_records_the_generation_too

run_mutation "generations: the publish insert dedup guard is dropped" \
  crates/geode-data/src/store/publish.rs \
  'where not exists (' \
  'where true or not exists (' \
  geode-data publishing_the_same_files_second_grain_does_not_duplicate_the_summary_row

run_mutation "generations: sweep reconciliation removed" \
  crates/geode-data/src/store/retention.rs \
  '    reconcile_generations(conn, ds)?;' \
  '    let _ = ds;' \
  geode-data sweeping_leaves_the_summary_matching_the_tables

run_mutation "generations: reconciliation covers only the first grain" \
  crates/geode-data/src/store/retention.rs \
  'let checks: Vec<String> = grains
        .iter()' \
  'let checks: Vec<String> = grains
        .iter()
        .take(1)' \
  geode-data a_generation_present_at_only_one_grain_survives_the_reconciliation

# Review round 1 (MAJ-2): `reconcile_generations` must derive its grain
# list from `ds.grains()` itself, never from whatever subset `sweep`'s
# caller happened to evict -- the entry above mutates the SQL builder's
# own truncation; this one mutates the call site that used to be (and
# must never again be) the caller-supplied `grains` parameter.
run_mutation "generations: reconciliation derives grains from the caller again, not the dataset" \
  crates/geode-data/src/store/retention.rs \
  'fn reconcile_generations(conn: &Connection, ds: &DatasetSpec) -> Result<(), StoreError> {
    let grains = ds.grains();' \
  'fn reconcile_generations(conn: &Connection, ds: &DatasetSpec) -> Result<(), StoreError> {
    let grains = vec![Grain::Position];' \
  geode-data reconciliation_covers_every_grain_the_dataset_has_not_just_the_swept_subset

run_mutation "generations: the open-time migration rebuild is skipped" \
  crates/geode-data/src/service.rs \
  '            if has_data {' \
  '            if false {' \
  geode-data open_rebuilds_the_summary_when_it_is_absent_and_data_exists

run_mutation "generations: resolve drops the dataset filter" \
  crates/geode-data/src/query/as_of.rs \
  'from generations where dataset = ? and source_time <= ?' \
  'from generations where (dataset = ? or true) and source_time <= ?' \
  geode-data resolve_reads_only_the_named_dataset

# Review round 1 (MIN-1): the outer `distinct` this branch added beyond
# the brief (`generations_union_sql` collapses one generation seen at N
# grains to one row, not just N per-table distincts unioned) had no
# entry, though the behaviour is real and the covering test already
# exists.
run_mutation "generations: the union across tables no longer collapses a generation seen at every grain" \
  crates/geode-data/src/store/ddl.rs \
  'format!("select distinct batch, book, gen_id, source_time from ({union})")' \
  'format!("select batch, book, gen_id, source_time from ({union})")' \
  geode-data rebuild_deduplicates_a_generation_shared_by_every_grains_tables

# Re-anchored (generations-table change): `resolve_generations` reads
# `generations`, not the raw archive, so the covering test needed to move
# with it -- `a_row_that_cannot_be_rebuilt_is_an_error_not_a_smaller_answer`
# (the old anchor's covering test, renamed) now errors during
# `rebuild_generations`'s insert, before this line ever runs, and no
# longer sees this mutation at all. A named filter is required here
# because the string this anchors is duplicated verbatim in the test-only
# `resolve_from_tables` oracle right below it in the file, and the two
# unfiltered occurrences are otherwise indistinguishable to the harness.
run_mutation "as-of: error propagation" \
  crates/geode-data/src/query/as_of.rs \
  'rows.collect::<Result<Vec<_>, _>>().map_err(err)' \
  'Ok(rows.filter_map(|r| r.ok()).collect())' \
  geode-data a_summary_row_that_cannot_be_read_is_an_error_not_a_smaller_answer

# Repaired (review round 1, MIN-7): the previous replacement here
# (`'as_of: None.or(compiled'`, no closing paren) never compiled, so
# `cargo test` failed at *compile* time and `run_mutation` reported
# "caught" on that non-zero exit -- not because any test saw the mutated
# behaviour. Verified pre-existing and identical on base commit
# `1046b0f` before this fix. The replacement below compiles and actually
# substitutes the requested instant for the resolved one -- exactly the
# defect this entry's name claims to guard against -- and is caught on
# *value* by the existing `a_historical_result_is_labelled_with_the_data_it_actually_read`
# (`left: Some("2026-08-30T00:00:00+00:00")` -- the request --
# `right: Some("2026-07-01T00:00:00+00:00")` -- the generation actually
# read), verified by hand.
run_mutation "provenance: resolved vs requested time" \
  crates/geode-data/src/service.rs \
  'AsOf::At(_) => Freshness {
                    dataset: dataset.clone(),
                    // The newest generation actually resolved, not the
                    // instant requested. Labelling every dataset with the
                    // request makes them all equal, and `stalest()` then
                    // cannot show that one side of a join is a month
                    // behind the other — which is all §5.4 is for.
                    as_of: compiled.resolved_as_of.get(dataset).map(|t| t.to_rfc3339()),
                    // Per-partition, so no single number describes it.
                    generation: 0,
                },' \
  'AsOf::At(t) => Freshness {
                    dataset: dataset.clone(),
                    as_of: Some(t.to_rfc3339()),
                    generation: 0,
                },' \
  geode-data a_historical_result_is_labelled_with_the_data_it_actually_read

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

# ---- the ingest queue dedupe (2026-09-07 display: 3051 generations of 17
# files that never changed — the ingest queue appended every poll's plan
# without deduplicating, and the runner reloaded every queued copy)

run_mutation "ingest: submit drops an item already queued for the same file" \
  crates/geode-data/src/ingest/runner.rs \
  '            existing.priority = existing.priority.min(item.priority);
            continue;' \
  '            existing.priority = existing.priority.min(item.priority);' \
  geode-data \
  enqueue_drops_an_item_already_queued_for_the_same_file

run_mutation "ingest: the runner re-checks change detection at pop time" \
  crates/geode-data/src/ingest/runner.rs \
  '        if stale {
            clear_in_flight(&queue);
            continue;
        }' \
  '        if false {
            clear_in_flight(&queue);
            continue;
        }' \
  geode-data \
  a_queued_item_whose_file_was_loaded_meanwhile_is_skipped_at_pop_time

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
  '    let key = ds.dimensions_at(grain);' \
  '    let key: Vec<&str> = grain.key_columns().to_vec();'

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
  '            let failed = sink(IngestEvent::Failed {
                dataset: item.dataset.clone(),
                batch: item.batch.clone(),
                reason: format!("dataset '"'"'{}'"'"' is not declared", item.dataset),
            });
            clear_in_flight(&queue);
            if !failed {
                return;
            }
            continue;' \
  '            clear_in_flight(&queue);
            continue;' \
  geode-data \
  an_item_naming_an_undeclared_dataset_fails_by_name_and_the_runner_continues

# ---- sources config (Phase 3 §5.2)
#
# Re-anchored (Phase 4c §2.2): `SourceSpec::from_doc` moved from
# geode-data/src/source/config.rs to geode-core/src/source_config.rs, so
# the covering tests moved with it and the entries below now run against
# geode-core, not geode-data — an unfiltered geode-data package check
# would find neither the mutated line nor the test that used to catch it.

run_mutation "sources: an undeclared dataset skips the source" \
  crates/geode-core/src/source_config.rs \
  '                Some(d) if schema.dataset(d).is_some() => d.to_string(),' \
  '                Some(d) => d.to_string(),' \
  geode-core \
  a_missing_or_unknown_dataset_is_an_error_and_the_source_is_skipped

run_mutation "sources: a pattern without a batch capture is dropped" \
  crates/geode-core/src/source_config.rs \
  '                    Ok(re) if re.capture_names().any(|c| c == Some("batch")) => Some(p.to_string()),' \
  '                    Ok(re) if re.capture_names().count() > 0 => Some(p.to_string()),' \
  geode-core \
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

# ---- keymap_edit: unbind (dialog interaction model task 2) --------------
#
# The dangerous branch: a wrong `true` deletes the user's own binding on
# that key instead of shadowing a desk one.
run_mutation "keymap_edit: unbind always removes instead of shadowing" \
  crates/geode-shell/src/keymap_edit.rs \
  '    let removed = if unbind.is_user_layer {' \
  '    let removed = if true {' \
  geode-shell \
  unbinding_a_lower_layer_binding_writes_a_none_shadow

run_mutation "keymap_edit: unbind always shadows instead of removing" \
  crates/geode-shell/src/keymap_edit.rs \
  '    let removed = if unbind.is_user_layer {' \
  '    let removed = if false {' \
  geode-shell \
  unbinding_a_user_layer_binding_removes_the_key

# Fix round 1, Critical: `bindings = [ { ... } ]` is a legal keymap
# document (build.rs reads it as a plain TOML array) but is not an
# `ArrayOfTables`, so the old code silently replaced it with an empty one
# and destroyed every binding on write. Disabling the shape guard falls
# through to treating it as already-fine, which this entry catches.
run_mutation "keymap_edit: bindings-not-an-array-of-tables is silently accepted" \
  crates/geode-shell/src/keymap_edit.rs \
  '        Some(item) if item.as_array_of_tables().is_none() => {' \
  '        Some(item) if false => {' \
  geode-shell \
  bindings_as_a_plain_array_is_rejected_without_touching_the_file

# Fix round 1, Important: `keys = { ... }` (an inline table) is also legal
# and also loaded fine by build.rs, but `Item::as_table_mut` returns `None`
# for one even though `is_table_like` already said yes — the old code
# panicked on exactly the input its own guard claimed to have handled.
# Reachable from a keystroke once Task 4 wires `d` to apply_unbind.
run_mutation "keymap_edit: keys_table_for panics on an inline keys table" \
  crates/geode-shell/src/keymap_edit.rs \
  '        .as_table_like_mut()' \
  '        .as_table_mut()' \
  geode-shell \
  keys_as_an_inline_table_does_not_panic_and_stays_editable

# Fix round 2, Important: TableLike::insert's occupied-entry branch resets
# the key's own formatting (entry.key_mut().fmt() strips a leading comment
# and reverts custom quoting), which round 1's index-to-insert conversion
# regressed for every already-present key a write touches. set_key must
# route an occupied key through get_mut, never insert.
run_mutation "keymap_edit: set_key always inserts instead of updating in place" \
  crates/geode-shell/src/keymap_edit.rs \
  '    if let Some(existing) = keys.get_mut(key) {' \
  '    if false {' \
  geode-shell \
  overwriting_an_existing_key_preserves_its_comment_and_quoting

# ---- keybinding dialog: the two modes (dialog interaction model task 3)
#
# The opening mode is one line, and every other test in that file either
# presses `/` first or uses keys both modes share — so the whole suite
# stays green with the dialog opening filter-first, which is exactly the
# behaviour this task removed. Only a test that asserts a bare letter did
# NOT reach the filter can see it.
run_mutation "keybindings: the dialog opens in filter mode" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '            mode: DialogMode::Normal,' \
  '            mode: DialogMode::Filter,' \
  geode-shell \
  the_dialog_opens_in_normal_mode_and_letters_do_not_type

# `EscapeStep::LeaveFilter`'s contract is that the query stays APPLIED —
# leaving a search leaves you on the match rather than undoing it. A
# dialog that cleared the query on the way out would still walk the same
# number of rungs and still close on the third press, so only an
# assertion on the query between rungs catches it.
run_mutation "keybindings: leaving filter mode clears the query" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '        state.mode = DialogMode::Normal;' \
  '        state.mode = DialogMode::Normal;
        state.query.clear();' \
  geode-shell \
  escape_walks_the_ladder_one_rung_at_a_time

# Review round 1, Important: the ClearQuery rung reset `selected` to 0
# without moving the viewport, which is parked wherever the *filtered*
# list left it — so row 0 painted above the top of the screen. Every
# state assertion in that file stays green with this deleted; only a test
# that asserts the row intersects the painted viewport sees it.
run_mutation "keybindings: clearing the query leaves the viewport parked" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '                    shell.keybindings_scroll.scroll_to_item(0);' \
  '' \
  geode-shell \
  clearing_the_query_scrolls_back_to_the_top

# Review round 1, Minor: a bare-only escape guard in normal mode sends
# `shift+escape` to the claim-and-drop arm, where it does nothing at all
# — `handle_key_down`'s own close never looked at modifiers. Nothing else
# in the suite presses a modified escape.
run_mutation "keybindings: escape only walks the ladder when unmodified" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '        if ks.key == "escape" {' \
  '        if ks.mods == Modifiers::NONE && ks.key == "escape" {' \
  geode-shell \
  a_modified_escape_walks_the_same_ladder_as_a_bare_one

# ---- keybinding dialog: the two verbs (dialog interaction model task 4)
#
# The capability the whole model exists to prove. Every entry below breaks
# one half of it; the two `is_user_layer` entries are the pair the spec's
# own risk list singles out ("unbind lowering to remove where it should
# shadow, which would delete a user's *other* binding rather than silence
# a desk one") — and its mirror, which entombs the user's own entry under
# a redundant `"none"` so the key stays dead with nothing in the file
# saying why.

run_mutation "keybindings: d never writes an unbind" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '            NormalCommand::Verb('"'"'d'"'"') => {' \
  '            NormalCommand::Verb('"'"'\0'"'"') => {' \
  geode-shell \
  d_unbinds_the_selected_binding

run_mutation "keybindings: r never writes a reset" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '            NormalCommand::Verb('"'"'r'"'"') => {' \
  '            NormalCommand::Verb('"'"'\u{1}'"'"') => {' \
  geode-shell \
  r_resets_a_user_override_by_removing_it

# `d` ignoring the row's layer, both directions. A `false` here shadows
# the user's own key with `"none"` instead of removing it; a `true`
# removes a key that lives in a layer this app never writes, so the
# builtin binding is left live and the user's file gains nothing.
run_mutation "keybindings: d shadows the user's own binding instead of removing it" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '        is_user_layer: bound.layer == Layer::User,' \
  '        is_user_layer: false,' \
  geode-shell \
  d_on_a_user_layer_binding_removes_it_rather_than_shadowing_it

run_mutation "keybindings: d removes where it should shadow a lower layer" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '        is_user_layer: bound.layer == Layer::User,' \
  '        is_user_layer: true,' \
  geode-shell \
  d_unbinds_the_selected_binding

# Reset is the removal branch by definition — a shadow would bury the
# very layer it was asked to uncover.
run_mutation "keybindings: r shadows instead of removing the user's override" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '        is_user_layer: true,' \
  '        is_user_layer: false,' \
  geode-shell \
  r_resets_a_user_override_by_removing_it

# Without the user-layer guard, `r` on a builtin row reaches
# `apply_unbind` with `is_user_layer: true` against a key that is not in
# the user file — which writes an otherwise-empty keymap.toml and, worse,
# lowers to a `"none"` shadow the moment the guard is relaxed the other
# way. Only a test that asserts NO file was written can see it.
# Re-anchored in fix round 1: the single `.filter(...)` refusal split
# into two branches, an unbound row (which HAS an override — the `"none"`
# shadow — and is told how to recover) and a live lower-layer binding
# (which genuinely has none). This entry defends the second guard.
run_mutation "keybindings: r writes on a row with no user override" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '    if bound.layer != Layer::User {' \
  '    if false {' \
  geode-shell \
  r_on_a_row_with_no_user_override_says_so_and_writes_nothing

# A notice reports on the keystroke (or click) that produced it. Left
# standing, it points at a row the user has since moved off — the footer
# lying about the current selection, which is worse than saying nothing.
# Re-anchored in fix round 1: the clear moved out of the normal-mode
# `match` and onto the two doors, because three other paths (the
# ClearQuery rung, the claim-and-drop early return, and the click) all
# changed the selection while leaving the complaint up.
run_mutation "keybindings: a notice outlives the keystroke it reports on" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '    if state.notice.take().is_some() {
        cx.notify();
    }
    let visible = visible_rows(state, &rows);

    if let Some(pending)' \
  '    let visible = visible_rows(state, &rows);

    if let Some(pending)' \
  geode-shell \
  a_notice_clears_on_the_next_normal_mode_keystroke

# The second door. A click never passes through `handle_key` at all, so
# the keystroke test above cannot see this one — and every mouse-driven
# selection change would leave the previous row's complaint standing.
run_mutation "keybindings: a click leaves the previous row's notice standing" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '    if state.notice.take().is_some() {
        cx.notify();
    }
    let visible = visible_rows(state, &rows);
    let Some(ix)' \
  '    let visible = visible_rows(state, &rows);
    let Some(ix)' \
  geode-shell \
  clicking_a_row_clears_a_standing_notice

# Fix round 1, Important 1. A row silenced by the user's own `d` HAS a
# user override — the `"none"` shadow is one — but derives as unbound, so
# the old single-branch refusal called it "no user override to reset".
# That is false, and it steers the user away from the one recovery that
# works. Every other assertion in the file stays green with the lie
# restored; only a test on the message itself sees it.
run_mutation "keybindings: r denies the user's own none shadow" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '        return Some(format!(
            "{} is unbound — if you silenced it, {RECOVERY}, or undo it \
             in keymap.toml if it was context-scoped",
            row.title
        ));' \
  '        return Some(format!("{} has no user override to reset", row.title));' \
  geode-shell \
  r_on_a_silenced_row_names_the_recovery_instead_of_denying_the_override

# Fix round 1, Important 3. `d` is one bare, unmodified key performing an
# immediate destructive disk write, and the row does not relabel until
# the ~500ms config watcher gets to it. Without the acknowledgement the
# keystroke is silent for half a second and never names the way back —
# and the file it wrote is identical either way, so only an assertion on
# the notice BEFORE `run_until_parked` catches it.
run_mutation "keybindings: d performs its write without acknowledging it" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '        .or_else(|| Some(format!("silencing {key} — {way_back}")))
' \
  '' \
  geode-shell \
  d_acknowledges_the_write_immediately_and_names_the_way_back

# Whole-branch review, Important 1. The `tab`/`shift+tab` reclaim was
# scoped to `"GeodeModal"`, which rides the modal PANEL and so is only on
# the dispatch stack while focus is inside it. Normal mode — the resting
# state this branch introduced — parks focus on the shell root, so
# `Root`'s window-wide Tab won and `focus_next` walked focus off the
# shell. This root-level context is the whole fix, and a green suite
# could not see its absence: every dialog assertion (the modal is open,
# the row is selected, the notice is right) survives a stray focus_next
# untouched. Only a test asserting the FOCUS STATE across a `tab` sees
# it.
run_mutation "dialog: tab escapes the modal in normal mode" \
  crates/geode-shell/src/shell/render.rs \
  '            .when(self.modal.is_some(), |el| el.key_context("GeodeModalOpen"))
' \
  '' \
  geode-shell \
  tab_in_normal_mode_leaves_focus_on_the_shell_root

# Whole-branch review, Important 2. `RECOVERY` ("press enter and type
# that key again") is exact only for a binding with NO context; for a
# contexted one `d`'s `"none"` lands in the contexted entry while the
# recovery rebind writes the no-context entry, so the promise is false.
# The mutation restores the single-sentence promise. Nothing about the
# FILE the write produces changes, so only an assertion on the message
# for a contexted row catches it.
run_mutation "keybindings: d promises the retype recovery for a contexted binding" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '        Some(_) => RECOVERY_CONTEXTED,' \
  '        Some(_) => RECOVERY,' \
  geode-shell \
  d_on_a_contexted_binding_does_not_promise_the_retype_recovery

# Whole-branch review, Minor 3, the `r` half. Its success path used to
# say nothing at all, so a reset whose `apply_unbind` came back
# `removed: false` looked exactly like one that worked — and even a
# working reset is invisible until the ~500ms watcher relabels the row.
# The disk write is identical either way; only an assertion on the notice
# before `run_until_parked` sees the silence.
run_mutation "keybindings: r resets without acknowledging it" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '        .or_else(|| Some(format!("removing your {key} override")))
' \
  '' \
  geode-shell \
  r_acknowledges_the_write_it_spawned

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
  '        let outgoing = std::mem::replace(&mut self.scope, scope);
        self.push_undo(outgoing);
        self.versions.scope += 1;' \
  '        let outgoing = std::mem::replace(&mut self.scope, scope);
        self.push_undo(outgoing);
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
  '        if !self.session_dirty && !frame_dirty && tiles == self.last_tiles_written {' \
  '        if !self.session_dirty && !frame_dirty {' \
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

run_mutation "perf: reset drops the previous-render timestamp so the first sample after it is fresh" \
  crates/geode-shell/src/shell/input.rs \
  '            self.last_render_started = None;
            cx.notify();' \
  '            cx.notify();' \
  geode-shell \
  reset_drops_the_previous_render_timestamp_so_the_first_sample_after_it_is_fresh

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

run_mutation "reload: ConfigReloaded is queued before ANY frame.update, including groupings_changed's and scopes_changed's (I2, residual fix)" \
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
            if scopes_changed {
                let saved = rebuild_saved_scopes(&self.services.config);
                self.frame.update(cx, |f, cx| {
                    if f.replace_saved_scopes(saved) {
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
            if scopes_changed {
                let saved = rebuild_saved_scopes(&self.services.config);
                self.frame.update(cx, |f, cx| {
                    if f.replace_saved_scopes(saved) {
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

run_mutation "frame: bar_model is rebuilt when versions change" \
  crates/geode-shell/src/frame.rs \
  '        if let Some((cached_versions, cached)) = self.bar_cache.borrow().as_ref()
            && *cached_versions == versions
        {
            return Rc::clone(cached);
        }' \
  '        if let Some((cached_versions, cached)) = self.bar_cache.borrow().as_ref()
            && *cached_versions != versions
        {
            return Rc::clone(cached);
        }' \
  geode-shell \
  the_bar_model_is_cached_on_versions_and_describes_the_scope

# ---- config_write: the one write door (Phase 4c task 1)
#
# These three guard the door every config write in geode-shell now goes
# through — the theme/fontsize/vimfind/frame/keymap persists and, from
# Phase 4c on, the config dialogs. A green suite sees none of them: each
# failure writes a file that parses fine and looks plausible, just to the
# wrong layer or over the user's own hand-edits.
#
# The layer guard is the only thing standing between a UI toggle and the
# shared desk layer. Defeating it makes every layer writable, which is
# silent: the write succeeds and the desk file is now the user's.
run_mutation "config_write: a non-user layer is writable" \
  crates/geode-shell/src/config_write.rs \
  '    if layer != Layer::User {' \
  '    if false {' \
  geode-shell \
  only_the_user_layer_is_writable

# The refusal a hand-edited-then-broken config depends on. Falling back to
# a fresh document instead of returning Err does not fail, warn, or crash
# — it silently replaces the user's file with an empty one on the next
# theme toggle.
run_mutation "config_write: an unparseable file is overwritten instead of refused" \
  crates/geode-shell/src/config_write.rs \
  '        text.parse::<DocumentMut>().map_err(|e| {
            format!(
                "failed to parse {}: {e} (file left untouched)",
                path.display()
            )
        })?' \
  '        text.parse::<DocumentMut>().unwrap_or_default()' \
  geode-shell \
  edit_refuses_an_unparseable_file_without_touching_it

# Re-anchored from theme.rs in Phase 4c task 1: `theme::write_atomic` and
# its `tmp_file_name` moved into config_write when the three write_atomic
# copies collapsed into one. Same M9 finding, same test, new home.
run_mutation "config_write: the temp name derives from the target file, not a hardcoded app.toml (M9)" \
  crates/geode-shell/src/config_write.rs \
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

run_mutation "commands: scope drop needs a dimension" \
  crates/geode-blotter/src/core/commands.rs \
  '                (Some("drop"), _) => Err("scope drop needs a dimension".into()),' \
  '                (Some("drop"), _) => Ok(Command::ScopeDrop(String::new())),' \
  geode-blotter new_scope_and_asof_forms_parse

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
                self.fill_window(w.start..end);
            }
        }
    }' \
  '        self.glyphs.clear();
    }' \
  geode-blotter \
  a_regroup_that_keeps_the_window_refills_it_immediately

run_mutation "delegate: invalidate_cells refills the requested window, not the shrunken cache window" \
  crates/geode-blotter/src/delegate.rs \
  '    fn invalidate_cells(&mut self) {
        let w = self.requested_window.clone();' \
  '    fn invalidate_cells(&mut self) {
        let w = self.cache.window();' \
  geode-blotter \
  a_window_shrunk_by_an_empty_snapshot_grows_back_when_rows_return

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
  '                self.error = Some(e);' \
  '                self.error = Some(e);
                self.table
                    .update(cx, |t, _| *t.delegate_mut() = BlotterDelegate::new());' \
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

run_mutation "blotter: :filter narrows only this tile" \
  crates/geode-blotter/src/tile.rs \
  '                self.tile_scope = scope;
                self.requery(cx);' \
  '                self.requery(cx);' \
  geode-blotter filter_narrows_only_this_tile_marks_it_and_round_trips_the_session

run_mutation "blotter: an unscoped tile keeps its own filter" \
  crates/geode-blotter/src/tile.rs \
  '                self.tile_scope.clone()' \
  '                Scope::default()' \
  geode-blotter an_unscoped_tile_still_applies_its_own_filter

run_mutation "blotter: filter validates against the dataset" \
  crates/geode-blotter/src/tile.rs \
  '                self.validate_tile_scope(&scope)?;' \
  '                let _ = self.validate_tile_scope(&scope);' \
  geode-blotter filter_validates_against_the_tiles_dataset

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

run_mutation "tile: completions offer dataset dimensions, not just displayed columns" \
  crates/geode-blotter/src/tile.rs \
  '                        ds.columns
                            .iter()
                            .filter(|c| names.contains(c.name.as_str()))
                            .map(|c| c.name.clone())
                            .collect()' \
  '                        Vec::new()' \
  geode-blotter \
  completions_offer_dataset_dimensions_not_just_displayed_columns

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

# Fix round 1, Finding 4 (harness ANCHOR-MISSING): the anchor above used
# to span the drain loop's ENTIRE match arm-by-arm, which is why it kept
# going stale — Task 2 added the `Distinct` arm, Task 3 changed
# `Published`, Task 5 replaced the inert `Distinct` arm with a real one,
# and each touch broke this entry even though none of them touched the
# behaviour it claims to guard. Re-anchored on the smallest fragment that
# still reproduces the pre-fix bug for one arm without naming the match
# at all: capture a SECOND, standalone `Entity<ShellView>` clone
# (`shell_direct`, taken once at `attach` time, independent of the
# window — the doc comment two lines up explains why that matters: an
# entity update through it succeeds forever, window or no) and route
# just the `Health` arm through it via a bare `cx.update`, short-
# circuiting with `continue` BEFORE the shared `window.update` call ever
# runs for that event. `Health` (not `Diagnostics`) is picked because
# it's what the covering test below already sends after closing the
# window, and neither arm is one a later Phase 4a task is expected to
# touch the way `Distinct`/`Published` were.
run_mutation "bridge: every event branch, not just Query, ends the drain task on a closed window" \
  crates/geode-app/src/bridge.rs \
  '    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut last_dropped = 0u64;
        while let Ok(event) = rx.recv().await {
            let now_dropped = dropped.load(Ordering::Relaxed);' \
  '    let shell_direct = shell.clone();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut last_dropped = 0u64;
        while let Ok(event) = rx.recv().await {
            let now_dropped = dropped.load(Ordering::Relaxed);
            if let DataEvent::Health { source, worst, detail } = &event {
                eprintln!("[data] health {source}: {} — {detail}", worst.label());
                shell_direct.update(cx, |s, cx| {
                    s.set_data_status(Some(format!("{source}: {}", worst.label())), cx)
                });
                last_dropped = now_dropped;
                continue;
            }' \
  geode-app \
  the_drain_task_ends_on_the_first_event_after_the_window_closes

# ---- carried dimensions (Phase 4 spec §3.3)

run_mutation "carried: a carried dimension is a payload column of its grain and finer" \
  crates/geode-data/src/ingest/split.rs \
  '    out.extend(
        ds.carried_dimensions_at(grain)
            .into_iter()
            .map(|c| c.name.as_str()),
    );' \
  '    let _ = ds.carried_dimensions_at(grain);' \
  geode-data a_carried_dimension_lands_in_its_grain_and_every_finer_one_but_not_position

run_mutation "carried: DDL carries a carried dimension" \
  crates/geode-data/src/store/ddl.rs \
  '    for c in ds.carried_dimensions_at(grain) {' \
  '    for c in ds.carried_dimensions_at(grain).into_iter().filter(|_| false) {' \
  geode-data create_table_carries_a_carried_dimension_at_its_grain_and_finer

run_mutation "carried: routing evaluates a carried dimension where it is carried" \
  crates/geode-data/src/query/scope_sql.rs \
  '    ds.carries(grain, base) || ds.column(base).and_then(|c| c.grain()) == Some(grain)' \
  '    grain.dimension_key_columns().contains(&base) || ds.column(base).and_then(|c| c.grain()) == Some(grain)' \
  geode-data a_selection_on_a_carried_dimension_is_direct_where_carried_and_probed_from_position

run_mutation "carried: attribution counts a carried dimension as part of the key" \
  crates/geode-core/src/attribution.rs \
  '    let key = ds.dimensions_at(grain);' \
  '    let key: Vec<&str> = grain.dimension_key_columns().to_vec();' \
  geode-core grouping_by_a_carried_dimension_is_additive_where_carried_and_non_attributable_where_not

run_mutation "carried: the grouping check accepts a carried dimension" \
  crates/geode-data/src/query/compile.rs \
  '    columns
        .iter()
        .all(|col| ds.carries(grain, dims.base_column(col)))' \
  '    columns
        .iter()
        .all(|col| grain.dimension_key_columns().contains(&dims.base_column(col)))' \
  geode-data grouping_by_a_carried_dimension_sums_like_the_key_it_depends_on_and_blanks_coarser_measures

run_mutation "carried: a dependency violation degrades health" \
  crates/geode-data/src/ingest/load.rs \
  '        if req
            .dataset
            .column(&c.column)
            .and_then(|col| col.carried_grain())
            .is_some()
        {' \
  '        if false {' \
  geode-data a_carried_dimension_dependency_violation_degrades_health

run_mutation "categorical: defaults on for dimensions, off otherwise" \
  crates/geode-core/src/schema/mod.rs \
  '    let categorical_default = matches!(role, ColumnRole::Dimension { .. });' \
  '    let categorical_default = true;' \
  geode-core categorical_defaults_true_for_dimensions_and_false_otherwise_and_attributes_may_opt_in

run_mutation "categorical: interning follows the flag" \
  crates/geode-data/src/store/ddl.rs \
  '    ds.categorical_columns()' \
  '    ds.columns.iter().filter(|c| matches!(c.role, ColumnRole::Dimension { .. })).map(|c| c.name.as_str()).collect()' \
  geode-data categorical_columns_follow_the_flag_not_the_role

run_mutation "schema: a bare dimension outside every key is dropped" \
  crates/geode-core/src/schema/mod.rs \
  '    ds.columns.retain(|c| !bare_outside_key.contains(&c.name));' \
  '    let _ = &bare_outside_key;' \
  geode-core a_bare_dimension_outside_every_built_in_key_is_an_error_and_is_dropped

run_mutation "schema: textual on an unroutable column is cleared" \
  crates/geode-core/src/schema/mod.rs \
  '        if unroutable.contains(&c.name) {' \
  '        if false {' \
  geode-core textual_on_a_column_no_grain_can_route_is_an_error_and_textual_is_cleared

run_mutation "schema: a dimension carried by an uncarriable grain is dropped" \
  crates/geode-core/src/schema/mod.rs \
  '        .retain(|c| !uncarriable.iter().any(|(name, _)| name == &c.name));' \
  '        .retain(|_| true);' \
  geode-core a_dimension_carried_by_the_pair_grain_is_uncarriable_and_is_dropped

# ---- text filter / ENUM dictionary rewrite, distinct (Phase 4 §3.4-3.5)

run_mutation "text: a categorical column matches the dictionary, not the rows" \
  crates/geode-data/src/query/scope_sql.rs \
  '            let test = if col.categorical && enum_types.contains(&ty) {' \
  '            let test = if false {' \
  geode-data a_text_filter_over_a_categorical_column_matches_the_dictionary_not_the_rows

# Root cause of the as-of slowdown (2026-09-07 fix): the rewrite is now
# gated on the type existing, not on the era, and the type is built to
# cover every era by `refresh_enum` reading live *and* archive. These two
# entries replace the old single "text: the rewrite is live-era only"
# entry, whose name and anchor described the gate this fix removed.

run_mutation "text: refresh_enum reads live and archive, not live alone" \
  crates/geode-data/src/store/ddl.rs \
  '             select distinct \"{column}\"::varchar from {archive_table}' \
  '             select distinct \"{column}\"::varchar from {live_table}' \
  geode-data a_value_dropped_from_a_republished_partition_still_shows_in_the_enum

run_mutation "text: the rewrite applies in every era, gated on the type alone" \
  crates/geode-data/src/query/scope_sql.rs \
  '        let enum_types: Vec<String> = cache.enum_types(conn, &ds.name)?.to_vec();' \
  '        let enum_types: Vec<String> = if era.kind == TableKind::Live { cache.enum_types(conn, &ds.name)?.to_vec() } else { Vec::new() };' \
  geode-data a_text_filter_over_a_categorical_column_selects_an_archived_only_value_under_as_of

run_mutation "text: the dictionary term keeps the escape clause" \
  crates/geode-data/src/query/scope_sql.rs \
  "t(v) where v ilike ? escape '\\\\'" \
  "t(v) where v ilike ?" \
  geode-data dictionary_and_row_scan_agree_for_any_needle

# Literal-list form (2026-09-07): the subquery form's OR of several
# correlated `IN (select ... enum_range ...)` terms still cost DuckDB a
# real planning-and-probing bill for a needle that matches nothing
# (docs/perf.md, "literal-list form"). `compile_scope` now resolves
# each categorical column's matches once at compile time and binds them
# as a literal list; these two entries anchor the two ways that could
# silently regress to "matches nothing selects everything" or "binds
# the wrong thing".

run_mutation "text: a needle matching no dictionary value collapses to false" \
  crates/geode-data/src/query/scope_sql.rs \
  '            r.direct.push(("false".to_string(), Vec::new()));' \
  '            {}' \
  geode-data a_needle_matching_no_dictionary_value_compiles_to_false

run_mutation "text: the literal list binds the matching values, not the pattern" \
  crates/geode-data/src/query/scope_sql.rs \
  'bound = Value::Text(matches.join(SELECTION_DELIMITER));' \
  'bound = pattern.clone();' \
  geode-data a_dictionary_match_binds_the_matching_values_not_the_pattern

# DictionaryCache (dictionary resolves once per statement, 2026-09-07):
# compile_scope re-ran existing_enum_types once per call and
# dictionary_matches once per categorical textual column per call, and
# compile_view called compile_scope once per measure grain plus once for
# the spine plus its own existing_enum_types for the interned-columns
# check -- 33 catalog round trips on the demo schema where 8 would do.
# DictionaryCache (above dictionary_matches) resolves each once per
# statement instead. The third entry below is anchored to
# compile_view_with_no_measures_resolves_the_dictionary_once_via_the_spine,
# not compile_view_over_two_grains_resolves_the_dictionary_once_per_statement
# as first sketched: verified by hand, a measure-grain loop running
# before the spine always pre-warms whatever the spine needs (the
# resolution is dataset-wide, not grain-specific), so a spine-only
# bypass is invisible to a lookups count taken after a statement with
# any measure grain finishes -- only the no-measures shape, where the
# spine is the sole (and first) consumer, catches it.

# Review round 1, Minor 4: `matches` is nested (`HashMap<String /*type*/,
# HashMap<String /*pattern*/, Vec<String>>>`) rather than a single
# `(type, pattern)`-keyed map, so a hit allocates nothing (only a miss
# needs to own the two strings it inserts). The three entries below are
# re-anchored against that shape; the mutations and the tests they name
# are unchanged in intent from before the nesting.

run_mutation "cache: a resolved dictionary match is reused, not re-fetched" \
  crates/geode-data/src/query/scope_sql.rs \
  '        if self
            .matches
            .get(enum_type)
            .and_then(|m| m.get(pattern))
            .is_none()
        {' \
  '        if true {' \
  geode-data a_cache_resolves_each_dictionary_once_per_statement

run_mutation "cache: matches are keyed by pattern, not the ENUM type alone" \
  crates/geode-data/src/query/scope_sql.rs \
  '        if self
            .matches
            .get(enum_type)
            .and_then(|m| m.get(pattern))
            .is_none()
        {
            let v = dictionary_matches(conn, enum_type, pattern)?;
            self.lookups += 1;
            self.matches
                .entry(enum_type.to_string())
                .or_default()
                .insert(pattern.to_string(), v);
        }
        Ok(self
            .matches
            .get(enum_type)
            .and_then(|m| m.get(pattern))
            .expect("just inserted this key"))' \
  '        if self
            .matches
            .get(enum_type)
            .and_then(|m| m.get(""))
            .is_none()
        {
            let v = dictionary_matches(conn, enum_type, pattern)?;
            self.lookups += 1;
            self.matches
                .entry(enum_type.to_string())
                .or_default()
                .insert(String::new(), v);
        }
        Ok(self
            .matches
            .get(enum_type)
            .and_then(|m| m.get(""))
            .expect("just inserted this key"))' \
  geode-data a_cache_keys_matches_by_pattern_not_type_alone

# Review round 1, Major 1: the mirror of the entry above — the ENUM-type
# half of the key, not the pattern half. Every other text-filter fixture
# declares exactly one categorical textual column, so a key collapsing
# to the pattern alone (dropping which column's dictionary it names) had
# nothing to collide with and no test could see it —
# `two_categorical_columns_fixture` (book + counterparty, one dictionary
# match each, to different values) exists so this mutation is reachable.
run_mutation "cache: matches are keyed by the ENUM type, not the pattern alone" \
  crates/geode-data/src/query/scope_sql.rs \
  '        if self
            .matches
            .get(enum_type)
            .and_then(|m| m.get(pattern))
            .is_none()
        {
            let v = dictionary_matches(conn, enum_type, pattern)?;
            self.lookups += 1;
            self.matches
                .entry(enum_type.to_string())
                .or_default()
                .insert(pattern.to_string(), v);
        }
        Ok(self
            .matches
            .get(enum_type)
            .and_then(|m| m.get(pattern))
            .expect("just inserted this key"))' \
  '        if self
            .matches
            .get("")
            .and_then(|m| m.get(pattern))
            .is_none()
        {
            let v = dictionary_matches(conn, enum_type, pattern)?;
            self.lookups += 1;
            self.matches
                .entry(String::new())
                .or_default()
                .insert(pattern.to_string(), v);
        }
        Ok(self
            .matches
            .get("")
            .and_then(|m| m.get(pattern))
            .expect("just inserted this key"))' \
  geode-data a_cache_keys_matches_by_type_not_pattern_alone

run_mutation "cache: compile_view's spine call uses the shared cache" \
  crates/geode-data/src/query/compile.rs \
  '        let spine_scope = compile_scope_cached(conn, scope, ds, spine_grain, dims, era, cache)?;' \
  '        let spine_scope = crate::query::scope_sql::compile_scope(conn, scope, ds, spine_grain, dims, era)?;' \
  geode-data compile_view_with_no_measures_resolves_the_dictionary_once_via_the_spine

# Review round 1, Minor 2: the interned-columns check's own cache use had
# no defence -- both compile_view.rs tests above already warm the cache
# through some other site before this one runs, so this mutation
# SURVIVED them (verified by hand: reverted to `existing_enum_types`
# directly, both still passed). Only a view with no text scope at all
# (so the text block never touches the cache) isolates it.
run_mutation "cache: the interned-columns check uses the shared cache" \
  crates/geode-data/src/query/compile.rs \
  '        let existing = cache.enum_types(conn, &view.dataset)?.to_vec();' \
  '        let existing = crate::store::ddl::existing_enum_types(conn, &view.dataset)?;' \
  geode-data compile_view_with_no_text_scope_resolves_the_dictionary_once_via_the_interned_check

# Re-anchored (review round 1): compile_distinct's body moved into
# compile_distinct_with_cache (Minor 5, below), and `&mut cache` became
# `cache` now that `cache` is itself the `&mut DictionaryCache`
# parameter, which also shortened the line to fit on one.
run_mutation "distinct: counts are taken under the given scope" \
  crates/geode-data/src/query/distinct.rs \
  '        let scope = compile_scope_cached(conn, &params.scope, ds, grain, dims, era.era(), cache)?;' \
  '        let scope = compile_scope_cached(conn, &geode_core::scope::Scope::default(), ds, grain, dims, era.era(), cache)?;' \
  geode-data distinct_counts_values_under_the_given_scope_and_unions_datasets

# Review round 1, Minor 5: the same call site as the entry above, but
# this one names the text-scope test, which is the only test exercising
# two datasets' dictionary resolves through one cache in the same call
# (two_dataset_fixture_with_textual_book). Bypassing the shared cache
# here is fully observable, unlike compile_view's grain loop: every
# dataset's ENUM type name is dataset-qualified, so there is no earlier
# call in the same statement that could pre-warm what a later one needs
# -- confirmed by hand, this mutation reads 0 instead of 4.
run_mutation "cache: compile_distinct's per-dataset call uses the shared cache" \
  crates/geode-data/src/query/distinct.rs \
  '        let scope = compile_scope_cached(conn, &params.scope, ds, grain, dims, era.era(), cache)?;' \
  '        let scope = crate::query::scope_sql::compile_scope(conn, &params.scope, ds, grain, dims, era.era())?;' \
  geode-data distinct_with_a_text_scope_over_two_datasets_resolves_each_dictionary_once

# Re-anchored (generations-table change): `era_for` dropped its unused
# `ds: &DatasetSpec` parameter once it stopped building a table list
# itself (`resolve_generations` now reads the summary by dataset name
# alone), so this call site's arity changed underneath the old anchor --
# a scoped run reported ANCHOR-MISSING rather than silently mutating the
# wrong thing.
run_mutation "distinct: as-of reads the archive era" \
  crates/geode-data/src/query/distinct.rs \
  '        let era = era_for(conn, &ds.name, &params.as_of)?;' \
  '        let era = era_for(conn, &ds.name, &geode_core::query::AsOf::Live)?;' \
  geode-data distinct_under_as_of_reads_the_archive_era

# D2 (final fix wave, T2 deferred): a derived dimension's own branch of
# `compile_distinct` had no test — `derived_case(d)` mutated to the base
# column's own varchar cast (what `None` already does) would silently
# return `book`'s source values instead of `desk`'s derived labels.
run_mutation "distinct: a derived dimension groups by its own labels, not the source column" \
  crates/geode-data/src/query/distinct.rs \
  '            Some(d) => crate::query::compile::derived_case(d),' \
  '            Some(_d) => format!("\"{base}\"::varchar"),' \
  geode-data compile_distinct_over_a_derived_dimension_groups_by_its_labels

# The brief's own suggested replacement — mutating the match arm's
# pattern and struct-literal head in one string — does not compile: it
# leaves `column: String::new()` and the later shorthand `column,` field
# both bound on one `DistinctOutcome`, which is `field column bound
# multiple times` (E0062), plus an unresolved `column` (E0425) since the
# pattern no longer binds it. Mutating the `values:` mapping to
# `Ok(Vec::new())` instead — the fallback the brief names for exactly
# this case — compiles and is what a delivered `Distinct` with the wrong
# payload actually looks like.
run_mutation "distinct: the sink maps a Distinct result to a Distinct event" \
  crates/geode-data/src/service.rs \
  '                    values: r.snapshot.map(|s| {
                        let v = s.column_index("value").expect("distinct selects value");
                        let n = s.column_index("n").expect("distinct selects n");
                        (0..s.rows())
                            .filter_map(|row| {
                                Some((s.text_at(v, row)?.to_string(), s.i64_at(n, row)? as u64))
                            })
                            .collect()
                    }),' \
  '                    values: Ok(Vec::new()),' \
  geode-data a_distinct_query_returns_value_counts_on_the_distinct_event

# --- Phase 4a Task 3: frame undo/redo, previous as-of, recent
# publishes, saved scopes, the bar model, session [frame] -------------

run_mutation "frame: undo is bounded" \
  crates/geode-shell/src/frame.rs \
  '        if self.scope_undo.len() > UNDO_DEPTH {' \
  '        if false {' \
  geode-shell undo_and_redo_walk_a_bounded_stack

run_mutation "frame: a new set clears redo" \
  crates/geode-shell/src/frame.rs \
  '        self.scope_redo.clear();' \
  '        let _ = &self.scope_redo;' \
  geode-shell undo_and_redo_walk_a_bounded_stack

run_mutation "frame: a text session pushes once" \
  crates/geode-shell/src/frame.rs \
  '            Some(None) => self.scope_session = Some(None),' \
  '            Some(None) => {
                let o = self.scope.clone();
                self.push_undo(o);
                self.scope_session = Some(None)
            }' \
  geode-shell a_text_session_coalesces_into_one_undo_entry

run_mutation "frame: as-of undo swaps rather than consumes" \
  crates/geode-shell/src/frame.rs \
  '        self.previous_as_of = Some(current);' \
  '        let _ = current;' \
  geode-shell as_of_remembers_one_previous_value_in_both_directions

run_mutation "frame: recent publishes are bounded and newest first" \
  crates/geode-shell/src/frame.rs \
  '        self.recent_publishes.push_front(publish);' \
  '        self.recent_publishes.push_back(publish);' \
  geode-shell recent_publishes_keep_the_last_thirty_two_newest_first

run_mutation "frame: the bar names a contradiction" \
  crates/geode-shell/src/scopebar.rs \
  '    let impossible = scope.impossible.then(|| {' \
  '    let impossible = false.then(|| {' \
  geode-shell a_contradiction_is_named_not_hidden

run_mutation "scopes: an unknown column drops the scope" \
  crates/geode-core/src/scopes.rs \
  '        if !bad.is_empty() {' \
  '        if false {' \
  geode-core a_scope_naming_an_unknown_column_is_dropped_with_a_warning

run_mutation "session: [frame] restores as-of" \
  crates/geode-shell/src/session.rs \
  '            t.insert("as_of".into(), toml::Value::String(at.to_rfc3339()));' \
  '            let _ = at;' \
  geode-shell a_frame_record_round_trips_through_session_toml_with_every_field

run_mutation "palette: selecting a saved scope loads it" \
  crates/geode-shell/src/shell/palette_ctl.rs \
  '                    if let Ok(true) = f.load_scope(&name) {' \
  '                    if let Ok(true) = f.load_scope("no-such-scope") {' \
  geode-shell a_saved_scope_appears_in_the_palette_and_selecting_it_loads_it

run_mutation "bar: a keystroke sets the frame text" \
  crates/geode-shell/src/shell/mod.rs \
  '                        if f.set_scope_in_session(s) {' \
  '                        if false && f.set_scope_in_session(s) {' \
  geode-shell typing_in_the_field_sets_the_frame_text_per_keystroke_and_enter_blurs

run_mutation "bar: escape restores the pre-focus text" \
  crates/geode-shell/src/shell/input.rs \
  '                if let Some(base) = self.filter_session_base.take() {' \
  '                if let Some(base) = self.filter_session_base.take().filter(|_| false) {' \
  geode-shell escape_restores_the_text_the_field_had_when_focused

run_mutation "bar: a chip close drops the dimension" \
  crates/geode-shell/src/shell/render.rs \
  '                    if f.drop_dimension(column) {' \
  '                    if false {' \
  geode-shell a_text_set_elsewhere_shows_in_the_field_and_a_chip_close_drops_the_dimension

run_mutation "keymap: mod = ctrl is refused" \
  crates/geode-shell/src/defaults.rs \
  '        Some("ctrl") => (' \
  '        Some("ctrl-never") => (' \
  geode-shell mod_alias_ctrl_is_refused_with_an_error_and_the_default_stands

# ---- Phase 4a Task 5: dimension pickers (spec §3.3-3.4)

run_mutation "picker: the request omits the column's own selection" \
  crates/geode-shell/src/shell/picker.rs \
  '    minus_own.dimensions.retain(|d| d.column != column);' \
  '    let _ = &minus_own;' \
  geode-shell the_picker_requests_values_minus_its_own_selection_and_applies_ticks_as_one_scope_change

run_mutation "picker: a stale outcome is dropped" \
  crates/geode-shell/src/shell/mod.rs \
  '        if *column != outcome.column || outcome.tag != state.tag {' \
  '        if *column != outcome.column || false {' \
  geode-shell a_stale_distinct_outcome_is_dropped

# Re-anchored: `apply` now decides its value list before this guard
# (`ticks_touched`, below), so the old `if !self.ticked.is_empty()`
# anchor is gone. Same behaviour under test: an emptied selection must
# drop the column rather than write an empty `DimensionSelection`, which
# `Scope` reads as "no constraint".
run_mutation "picker: an empty tick set drops the chip" \
  crates/geode-shell/src/shell/picker.rs \
  '        if !values.is_empty() {' \
  '        if true {' \
  geode-shell apply_replaces_the_columns_selection_and_an_empty_tick_set_drops_it

# The single-value flow. Collapse the untouched branch so an empty tick
# set always means "select nothing" again — the exact defect this fixed:
# arrow to a value, press enter, and the modal closes having changed
# nothing (the scope equals itself, so `Frame::set_scope` returns false).
run_mutation "picker: enter on an untouched tick set commits nothing" \
  crates/geode-shell/src/shell/picker.rs \
  '        let values: Vec<String> = if self.ticked.is_empty() && !self.ticks_touched {' \
  '        let values: Vec<String> = if false {' \
  geode-shell enter_on_an_untouched_tick_set_commits_the_highlighted_value

# The other half of the same distinction, and the one that matters for
# PHILOSOPHY's keyboard-reach rule: drop `clear`'s flag write and
# `ctrl+x` then `enter` stops clearing a dimension — it commits the
# highlighted value instead, leaving no keyboard route to clearing one
# (the chip's close glyph is mouse-only).
run_mutation "picker: ctrl+x no longer counts as touching the tick set" \
  crates/geode-shell/src/shell/picker.rs \
  '    pub fn clear(&mut self) {
        self.ticks_touched = true;' \
  '    pub fn clear(&mut self) {' \
  geode-shell an_explicit_clear_makes_enter_drop_the_selection

# "Nothing is configured to pick" and "your filter matched nothing" must
# not print the same string: the first is why an unconfigured `alt+p`
# reads as a broken picker rather than an empty one. Collapse the
# columns stage's empty-pickable arms back onto "no matches".
run_mutation "picker: the columns stage's empty states print the same string" \
  crates/geode-shell/src/shell/picker.rs \
  '        (false, _) => "no matches",' \
  '        (false, _) | (true, _) => "no matches",' \
  geode-shell the_empty_states_say_which_emptiness_it_is

# Review finding: the two reasons `pickable` comes out empty are
# unrelated — no `datasets` doc, or a doc declaring no categorical
# columns — and blaming a missing file for the second sends the reader
# to the wrong place. Serve the missing-file message for both.
run_mutation "picker: a loaded schema is reported as a missing file" \
  crates/geode-shell/src/shell/picker.rs \
  '        (true, true) => "nothing to pick — this schema declares no categorical columns",' \
  '        (true, true) => "nothing to pick — no datasets config is loaded",' \
  geode-shell the_empty_states_say_which_emptiness_it_is

# Review finding: the values stage's own branch had no entry of its
# own, only the columns stage's. A distinct query that returned no rows
# at all is a fact about the data under the current scope, not about the
# filter the user typed.
run_mutation "picker: the values stage's empty states print the same string" \
  crates/geode-shell/src/shell/picker.rs \
  '    if values_is_empty {
        "no values in scope"' \
  '    if false {
        "no values in scope"' \
  geode-shell the_empty_states_say_which_emptiness_it_is

# Review finding: `tab`/`ctrl+a` set `ticks_touched` ahead of their own
# guards, so a keystroke that visibly ticked nothing (a filter matching
# nothing, or values not yet delivered) permanently disarmed the
# highlight-commit path — the original defect by another route. Hoist
# the flag back above the guard.
run_mutation "picker: a tab that ticks nothing still counts as a touch" \
  crates/geode-shell/src/shell/picker.rs \
  '    pub fn toggle_selected(&mut self) {
        let shown = self.shown();' \
  '    pub fn toggle_selected(&mut self) {
        self.ticks_touched = true;
        let shown = self.shown();' \
  geode-shell a_tick_keystroke_that_ticks_nothing_does_not_count_as_touching

run_mutation "picker: a ctrl+a over nothing still counts as a touch" \
  crates/geode-shell/src/shell/picker.rs \
  '        if shown.is_empty() {
            return; // see `toggle_selected` on why this is not a touch
        }' \
  '' \
  geode-shell a_tick_keystroke_that_ticks_nothing_does_not_count_as_touching

# `tab` is the only key that selects a value, and the footer is the only
# place that says so. Serve the columns stage's vocabulary to both.
run_mutation "picker: the values stage stops advertising tab" \
  crates/geode-shell/src/shell/picker.rs \
  '        Stage::Values { .. } => &[
            Hint::Text("type to filter ·"),
            Hint::Key("up"),
            Hint::Key("down"),
            Hint::Text("move ·"),
            Hint::Key("tab"),' \
  '        Stage::Values { .. } => &[
            Hint::Text("type to filter ·"),
            Hint::Key("up"),
            Hint::Key("down"),
            Hint::Text("move ·"),
            Hint::Key("enter"),' \
  geode-shell each_stage_advertises_its_own_vocabulary

# The hint row must actually paint. Drop it from `build` and the covering
# test's `debug_bounds("picker-hints")` must go `None` at both stages.
run_mutation "picker: the footer hint never paints" \
  crates/geode-shell/src/shell/picker.rs \
  '        .child(hint_row(
            &picker.stage,
            theme.muted_foreground,
            theme.muted,
            theme.border,
        ))' \
  '' \
  geode-shell arrowing_to_a_value_and_pressing_enter_commits_it_without_tab

run_mutation "pickable: keys are not pickable" \
  crates/geode-shell/src/shell/mod.rs \
  '        for column in dataset.categorical_columns() {' \
  '        for column in dataset.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>() {' \
  geode-shell pickable_columns_are_every_categorical_column_plus_derived_dimensions

# Fix round 1, Finding 1: the values `uniform_list` must scroll to follow
# `selected` past `palette::VISIBLE_ROWS`, or keyboard navigation goes
# blind — the highlight leaves the viewport with only its index having
# changed. Drop the actual `scroll_to_item` call `sync_picker_scroll`
# makes (keeping `picker.selected`/`ScrollStrategy` referenced so the
# mutant still compiles) and the covering test's `debug_bounds` on the
# now off-screen (and, since `uniform_list` truly virtualizes, likely
# unpainted) row 20 must fail.
run_mutation "picker: the values list never scrolls to follow the selection" \
  crates/geode-shell/src/shell/picker.rs \
  '        shell
            .picker_scroll
            .scroll_to_item(picker.selected, ScrollStrategy::Nearest);' \
  '        let _ = (&shell.picker_scroll, picker.selected, ScrollStrategy::Nearest);' \
  geode-shell keyboard_navigation_past_visible_rows_scrolls_the_selection_into_view

# Fix round 1, Finding 3: a scope chip's body click had no test — only
# the close glyph's `on_chip_close` was e2e-covered. Drop the actual
# `picker::open` call `on_chip_open`'s closure makes; the covering test's
# real mouse click on `scope-chip-book` must then fail to open the modal.
run_mutation "toolbar: the chip body click never opens the picker" \
  crates/geode-shell/src/shell/render.rs \
  '            chip_open_entity.update(cx, |view, cx| {
                picker::open(view, Some(column), window, cx);
            });' \
  '            let _ = (&chip_open_entity, &column, &window, &cx);' \
  geode-shell clicking_a_scope_chips_body_opens_the_picker_on_that_column

# ---- Phase 4a Task 6: the as-of selector and the historical indicator
# (spec §3.6, §3.11)

run_mutation "as-of: a bad time never sets the frame" \
  crates/geode-shell/src/shell/asof_view.rs \
  '            Err(msg) => {
                if let Some(state) = shell.as_of_dialog.as_mut() {
                    state.error = Some(msg);
                    state.resolved = None;
                }
                cx.notify();
            }' \
  '            Err(msg) => {
                if let Some(state) = shell.as_of_dialog.as_mut() {
                    state.error = Some(msg);
                    state.resolved = None;
                }
                shell.frame.update(cx, |f, cx| {
                    if f.set_as_of(AsOf::At(Utc::now())) {
                        cx.notify();
                    }
                });
            }' \
  geode-shell a_bad_time_shows_inline_and_enter_does_nothing

run_mutation "as-of: the stripe is painted only when historical" \
  crates/geode-shell/src/shell/render.rs \
  '        let is_historical = matches!(self.frame.read(cx).as_of(), AsOf::At(_));' \
  '        let is_historical = true;' \
  geode-shell typing_a_time_and_enter_sets_as_of_and_paints_the_stripe_and_segment

# ---- Phase 4a Task 8: the flip barrier (spec §3.10)

run_mutation "flip: failure counts as arrival" \
  crates/geode-blotter/src/tile.rs \
  '                self.frame.update(cx, |f, cx| {
                    if f.arrived(key, acted) {
                        cx.notify();
                    }
                });' \
  '                let _ = (key, acted);' \
  geode-blotter two_tiles_promote_in_the_same_pass_and_a_failure_releases_the_barrier

run_mutation "flip: a staged snapshot waits for the barrier" \
  crates/geode-blotter/src/tile.rs \
  '                if wants {' \
  '                if false {' \
  geode-blotter two_tiles_promote_in_the_same_pass_and_a_failure_releases_the_barrier

run_mutation "flip: the deadline releases" \
  crates/geode-shell/src/frame.rs \
  '            Some(b) if now.duration_since(b.opened) >= FLIP_DEADLINE => {
                self.release();
                true
            }' \
  '            Some(_) if false => {
                self.release();
                true
            }' \
  geode-shell the_deadline_releases_with_whatever_arrived

run_mutation "flip: only scope/grouping/as-of open a barrier" \
  crates/geode-shell/src/shell/mod.rs \
  '        if now_v.scope != last.scope || now_v.grouping != last.grouping || now_v.as_of != last.as_of
        {' \
  '        if now_v != last
        {' \
  geode-shell a_data_bump_opens_no_barrier

run_mutation "flip: a non-following tile still arrives on its own" \
  crates/geode-blotter/src/tile.rs \
  '            if self.frame.read(cx).barrier_wants(key, now) {' \
  '            if false {' \
  geode-blotter a_pinned_tile_arrives_from_on_frame_changed_without_requerying

# ---- Fix round 1, Finding 1: a staged snapshot needs its own version
# identity (crates/geode-blotter/src/tile.rs's `staged`/`promote`/
# `requery`) — a second scope/grouping/as-of mutation within the same
# 250ms window must never let a `flip` bump promote a snapshot staged
# for the wrong (older) versions. Both entries are filtered to the one
# test with all three race shapes (Part 1: both fixes independently
# sufficient; Part 2: only `promote`'s version check defends a pinned
# tile that never requeries; Part 3: only `requery`'s clear defends a
# data-only bump the barrier's own versions never move for) — verified
# by hand that each mutation is actually caught by this test (not just
# by the harness's own bug-reproduction run), console output in
# task-8-report.md's "Fix round 1" section.

# Re-anchored (F5, final fix wave): `promote`'s inline three-field
# compare now goes through `FrameVersions::same_flip_identity` (also
# used by `Frame::matches`), so the mutation targets that call instead.
run_mutation "flip: promote only applies a staged snapshot for the versions it was staged under" \
  crates/geode-blotter/src/tile.rs \
  '        if versions.same_flip_identity(now) {' \
  '        if true {' \
  geode-blotter a_second_mutation_during_a_barrier_wait_clears_the_stale_staged_snapshot

run_mutation "flip: a fresh requery clears whatever was staged before it" \
  crates/geode-blotter/src/tile.rs \
  '        self.staged = None;
' \
  '' \
  geode-blotter a_second_mutation_during_a_barrier_wait_clears_the_stale_staged_snapshot

# ---- Final fix wave (whole-branch review, 2026-09-06): F1, F3 ---------

# F1: `HH:MM`/`HH:MM:SS` used to resolve on UTC's date
# (`now.date_naive().and_time(t).and_utc()`); they now resolve on the
# LOCAL date and map to UTC (one clock throughout — the modal's presets,
# preview, the scope bar and the status segment all already showed
# local time). The pinned test computes its own expectation independent
# of `parse_as_of` so it catches a regression to the old UTC-resolving
# behaviour on any machine whose local zone differs from UTC.
run_mutation "as-of: HH:MM resolves on the trader's local date, not UTC's" \
  crates/geode-core/src/query.rs \
  '    if let Ok(t) = NaiveTime::parse_from_str(text, "%H:%M") {
        return resolve_local(today_local, t, text);
    }' \
  '    if let Ok(t) = NaiveTime::parse_from_str(text, "%H:%M") {
        return Ok(now.date_naive().and_time(t).and_utc());
    }' \
  geode-core as_of_resolves_on_the_local_date_not_utcs

# F3: `DataHandle::distinct`'s refusal (`false`: the queue is full or the
# service thread is gone) used to be discarded — nothing else would ever
# reply, so a refused picker request stayed on "loading…" forever. A
# synthetic error outcome is now delivered right at the refusal site.
run_mutation "bridge: a refused distinct request errors the picker instead of leaving it loading forever" \
  crates/geode-app/src/bridge.rs \
  '                if !queued {' \
  '                if false {' \
  geode-app a_refused_distinct_request_errors_the_picker

if [[ -n "$changed_ref" ]]; then
  echo "skipped $skipped entries whose files are unchanged since $changed_ref"
fi
