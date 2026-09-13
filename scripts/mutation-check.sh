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
# Usage: zsh scripts/mutation-check.sh [--anchors-only] [--changed[=REF]] [substring]
#   (from the repo root)
#
# --anchors-only runs no cargo at all: it checks every selected entry's
# anchor against its file and reports the ones that no longer match
# (ANCHOR) or match more than once (AMBIG), then a one-line summary.
# Under a second over the whole file (one python pass, each source file
# read once). Exits non-zero on any finding, so it can gate a merge; a
# selection that matches nothing says so rather than passing. Run it
# before every merge and after
# any edit near an anchored line — a normal run reports these two only
# for the entries it happens to select, and an ambiguous anchor is the
# quiet one: `replace(..., 1)` mutates the FIRST match, so an entry whose
# anchor is duplicated by a later verbatim reuse (the final review of the
# health follow-ups found exactly that — a seed loop copied the ingest
# sink's emit closure, and the entry guarding the sink mutated the seed
# instead) keeps printing "caught" while defending nothing. A normal run
# now prints AMBIG for the entries it does select, and still mutates the
# first match.
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
# filter is why every entry now names one. The last 79 that did not were
# filled in by probing each mutation against the full suite and reading
# back which test failed; a `--changed` run over service.rs and catalog.rs
# had been taking over an hour on those alone. An entry added from here on
# names its test too: without one, "caught" says nothing about WHICH test
# saw the mutation, which is the "two defences overlapping" lie the header
# above warns about. `.cargo/config.toml`
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
# --anchors-only collects (name, file, anchor) NUL-separated here and
# checks them all in one pass at the end.
anchors="$(mktemp -t mutate-anchors)"
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
  rm -f "$bak" "$log" "$anchors"
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
anchors_only=0
if [[ "${1:-}" == --anchors-only ]]; then
  anchors_only=1
  shift
fi
changed_ref=""
if [[ "${1:-}" == --changed ]]; then
  changed_ref="main"
  shift
elif [[ "${1:-}" == --changed=* ]]; then
  changed_ref="${1#--changed=}"
  shift
fi
only="${1:-}"
if [[ "$only" == --* ]]; then
  # Flags are positional: --anchors-only first, then --changed, then the
  # substring. A flag in the wrong slot used to become the substring, match
  # no entry, and exit 0 having checked nothing.
  echo "usage: zsh scripts/mutation-check.sh [--anchors-only] [--changed[=REF]] [substring]" >&2
  echo "unexpected argument in the substring slot: $only" >&2
  exit 2
fi
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
  if (( anchors_only )); then
    printf '%s\0%s\0%s\0' "$name" "$file" "$from" >> "$anchors"
    return 0
  fi
  # A moved or deleted file is a stale entry, reported by name, not a
  # traceback that ends the run (`set -e` would otherwise stop here).
  if [[ ! -f "$file" ]]; then
    echo "ANCHOR    $name  <-- file missing: $file"
    return 0
  fi
  # How many times the anchor occurs, checked before anything is written.
  # 0 is a stale entry; more than 1 is an ambiguous one, and both are
  # findings whether or not cargo runs afterwards. Declared and assigned
  # on separate lines on purpose: `local hits=$(…)` would mask python's
  # exit status, and a failure would then read as "0 hits".
  local hits
  hits=$(python3 - "$file" "$from" <<'PY'
import sys, pathlib
print(pathlib.Path(sys.argv[1]).read_text().count(sys.argv[2]))
PY
  ) || hits=-1
  if (( hits < 0 )); then
    echo "ANCHOR    $name  <-- could not read $file"
    return 0
  fi
  if (( hits == 0 )); then
    # A stale anchor is a finding in its own right: the mutation no longer
    # names live code. It is not a reason to abort mid-run with the tree
    # half-mutated.
    echo "ANCHOR    $name  <-- anchor no longer matches; mutation is stale"
    return 0
  fi
  if (( hits > 1 )); then
    echo "AMBIG x$hits  $name  <-- anchor matches $hits times; only the first is mutated"
  fi
  cp "$file" "$bak"
  in_flight="$file"
  python3 - "$file" "$from" "$to" <<'PY'
import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
p.write_text(s.replace(sys.argv[2], sys.argv[3], 1))
PY
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
  '    if false {' \
  geode-data \
  a_sentinel_older_than_its_csv_means_the_file_is_being_rewritten

run_mutation "discovery: no sentinel means pending, not ready" \
  crates/geode-data/src/source/discovery.rs \
  '    let Ok(sentinel_meta) = std::fs::metadata(sentinel_path) else {' \
  '    let Ok(sentinel_meta) = std::fs::metadata(csv_path) else {' \
  geode-data \
  a_csv_without_a_sentinel_is_pending_not_broken

run_mutation "discovery: waiting too long is reported, not waited on forever" \
  crates/geode-data/src/source/discovery.rs \
  '        return Ok(if waited > spec.pending_timeout {' \
  '        return Ok(if false {' \
  geode-data \
  pending_past_the_timeout_becomes_pending_too_long

run_mutation "discovery: an unimplemented readiness strategy is surfaced" \
  crates/geode-data/src/source/discovery.rs \
  '    if let Readiness::StableMtime { polls } = spec.readiness {' \
  '    if let Readiness::StableMtime { polls } = Readiness::Sentinel {' \
  geode-data \
  an_unimplemented_readiness_strategy_says_so_instead_of_going_quiet

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
    };' \
  geode-data \
  a_malformed_sentinel_is_orphaned_with_the_reason

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
  'table_name(&ds.name, probe, TableKind::Live),' \
  geode-data \
  an_as_of_query_reads_only_the_archive_even_through_a_semi_join

run_mutation "as-of: ENUM cast era guard" \
  crates/geode-data/src/query/compile.rs \
  'if era.kind != TableKind::Live {' \
  'if false {' \
  geode-data \
  as_of_survives_a_value_that_has_since_left_live

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

# Anchored through the two lines below the tie-break, not the tie-break
# alone: the test-only `resolve_from_tables` oracle repeats the same
# window clause verbatim, and `from generations where dataset = ?` is the
# nearest line that tells the real resolve apart from it. The intended
# site is `resolve_generations` -- the oracle is what the tests compare
# against, so mutating it would break the comparison from the wrong end.
run_mutation "as-of: source-time tie breaks on gen_id" \
  crates/geode-data/src/query/as_of.rs \
  '                        order by source_time desc, gen_id desc
                    ) as rn
             from generations where dataset = ? and source_time <= ?' \
  '                        order by source_time desc
                    ) as rn
             from generations where dataset = ? and source_time <= ?' \
  geode-data \
  a_tie_on_source_time_resolves_to_the_newest_gen_id_every_time

# Retention deletes. A wrong query shows a wrong number and can be
# re-run; a wrong sweep destroys history that no longer exists to be
# re-read. These entries are here because this file had exactly one, and
# it is the least recoverable code in the data layer.

run_mutation "retention: an empty policy evicts nothing" \
  crates/geode-data/src/store/retention.rs \
  '        if !policy.is_empty() {' \
  '        if true {' \
  geode-data \
  an_empty_policy_evicts_nothing

run_mutation "retention: the bookless partition is matchable by its keys" \
  crates/geode-data/src/store/retention.rs \
  '                       and k.book is not distinct from a.book' \
  '                       and k.book = a.book' \
  geode-data \
  a_null_book_does_not_disable_the_whole_sweep

run_mutation "retention: age keeps the recent, not the ancient" \
  crates/geode-data/src/store/retention.rs \
  '                    "source_time >= '"'"'{}'"'"'::timestamptz",' \
  '                    "source_time <= '"'"'{}'"'"'::timestamptz",' \
  geode-data \
  keep_by_age_evicts_on_source_time

run_mutation "retention: every configured rule must be satisfied" \
  crates/geode-data/src/store/retention.rs \
  'keep = keep.join(" and "),' \
  'keep = keep.join(" or "),' \
  geode-data \
  both_policies_apply_together

run_mutation "retention: the generation count bound is what it says" \
  crates/geode-data/src/store/retention.rs \
  'keep.push(format!("rn <= {n}"));' \
  'keep.push(format!("rn <= {}", n + 1));' \
  geode-data \
  keep_by_count_is_per_partition

run_mutation "retention: the remaining bound is the oldest, not the newest" \
  crates/geode-data/src/store/retention.rs \
  '            (Some(a), Some(b)) => Some(a.min(b)),' \
  '            (Some(a), Some(b)) => Some(a.max(b)),' \
  geode-data \
  the_oldest_remaining_bound_spans_every_grain_swept

run_mutation "retention: eviction is counted from the rows actually removed" \
  crates/geode-data/src/store/retention.rs \
  'report.evicted_rows += (before - after).max(0) as usize;' \
  'report.evicted_rows += 0;' \
  geode-data \
  keep_by_count_is_per_partition

run_mutation "retention: source-time tie breaks on gen_id" \
  crates/geode-data/src/store/retention.rs \
  'order by source_time desc, gen_id desc' \
  'order by source_time desc' \
  geode-data \
  a_tie_on_source_time_keeps_the_generation_as_of_would_pick

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
# `resolve_from_tables` oracle right below it in the file. The anchor now
# carries the last line of the comment above the real one, so it matches
# `resolve_generations` and nothing else; the filter stays anyway, since
# it is the test that proves the entry guards what its name says.
run_mutation "as-of: error propagation" \
  crates/geode-data/src/query/as_of.rs \
  '    // query that answers with less data than it should and says nothing.
    rows.collect::<Result<Vec<_>, _>>().map_err(err)' \
  '    // query that answers with less data than it should and says nothing.
    Ok(rows.filter_map(|r| r.ok()).collect())' \
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
  '                    let _ = oldest;' \
  geode-data \
  an_as_of_join_labels_each_side_with_the_instant_it_actually_read

# The spine's own resolve, not the join's: `era_for` and the joined-dataset
# arm below it pick the oldest the same way, so the anchor carries the
# `resolve_generations` call above it, which names `dataset` in one and
# `&join.dataset` in the other. The join site has its own entry
# ("provenance: a join is labelled with its own instant").
run_mutation "provenance: stalest partition, not newest" \
  crates/geode-data/src/query/compile.rs \
  '            let gens = crate::query::as_of::resolve_generations(conn, dataset, *t)?;
            if let Some(oldest) = gens.iter().map(|g| g.source_time).min() {' \
  '            let gens = crate::query::as_of::resolve_generations(conn, dataset, *t)?;
            if let Some(oldest) = gens.iter().map(|g| g.source_time).max() {' \
  geode-data \
  as_of_after_the_current_generation_reads_the_current_generation

run_mutation "enum: a stale value degrades rather than failing the query" \
  crates/geode-data/src/query/compile.rs \
  'selects.push(format!("try_cast(s.\"{g}\" as {ty}) as \"{g}\""));' \
  'selects.push(format!("s.\"{g}\"::{ty} as \"{g}\""));' \
  geode-data \
  a_stale_enum_degrades_that_column_instead_of_failing_the_query

run_mutation "scope: an ordering comparison on a derived dimension is caught at entry" \
  crates/geode-core/src/scope/mod.rs \
  'if dims.get(column).is_some() && !matches!(op, CompareOp::Eq | CompareOp::Ne) {' \
  'if false {' \
  geode-core \
  an_ordering_comparison_on_a_derived_dimension_is_caught_at_entry

run_mutation "scope: a contradiction still names its dimension" \
  crates/geode-core/src/scope/mod.rs \
  'dimensions.retain(|d| !d.values.is_empty() || contradicted.contains(&d.column));' \
  'dimensions.retain(|d| !d.values.is_empty());' \
  geode-core \
  a_contradiction_still_names_the_dimension_that_caused_it

# ---- derived column attribution (spec §6.3)

run_mutation "derived: a derived column inherits its inputs' attribution" \
  crates/geode-data/src/query/compile.rs \
  '            let referenced = referenced_columns(sql, &columns);' \
  '            let referenced: Vec<&CompiledColumn> = Vec::new();' \
  geode-data \
  a_derived_column_inherits_the_attribution_of_what_it_references

run_mutation "derived: attribution meet takes the weaker claim" \
  crates/geode-core/src/attribution.rs \
  '            (NonAttributable, _) | (_, NonAttributable) => NonAttributable,' \
  '            (NonAttributable, _) | (_, NonAttributable) => Additive,' \
  geode-core \
  the_attribution_meet_takes_the_weaker_claim

run_mutation "derived: a name inside a string literal is not a reference" \
  crates/geode-data/src/query/compile.rs \
  '        if in_string {
            continue;
        }' \
  '        if false {
            continue;
        }' \
  geode-data \
  a_column_name_inside_a_string_literal_is_not_a_reference

# ---- config validation (spec §10.1, §6.8)

run_mutation "validation: views are checked when the service opens" \
  crates/geode-data/src/service.rs \
  '            .flat_map(|v| v.validate(&config.schema, &config.dimensions))' \
  '            .flat_map(|_v| Vec::<Diagnostic>::new())' \
  geode-data \
  a_misconfigured_view_is_a_diagnostic_at_open_not_a_binder_error_later

run_mutation "validation: a scope column is checked against the dataset" \
  crates/geode-core/src/scope/mod.rs \
  '                None if ds.column(&c).is_none() => {' \
  '                None if false => {' \
  geode-core \
  validation_rejects_unknown_columns_with_a_diagnostic

run_mutation "validation: a derived dimension is not an unknown grouping" \
  crates/geode-core/src/view.rs \
  '            if let Some(d) = dims.get(g) {' \
  '            if let Some(d) = None::<&crate::dimensions::DerivedDimension> {' \
  geode-core \
  grouping_by_a_derived_dimension_is_not_an_unknown_column

run_mutation "validation: a derived dimension shadowing a column is reported" \
  crates/geode-core/src/view.rs \
  '                if ds.column(g).is_some() {' \
  '                if false {' \
  geode-core \
  a_derived_dimension_that_shadows_a_real_column_is_reported

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

# The plain-measure half of the §6.3 blanking, the pair to the derived
# entry below. A measure whose grain cannot be attributed at this depth is
# emitted as `case when s.row_depth in (…) then null else agg."x" end`;
# without it the cell shows the value belonging to an ANCESTOR row, which
# is the double-count §6.3 exists to prevent, and it shows it as an
# ordinary number with no marker a reader could notice. Added 2026-09-09:
# until then the entry below was anchored on the bare
# `let expr = if blank.is_empty() {`, whose first occurrence is this site,
# so re-aiming it at the derived site left this one bare.
run_mutation "measure: a non-attributable measure is blanked, not an ancestor's number" \
  crates/geode-data/src/query/compile.rs \
  '                .filter(|d| by_depth[*d] == Attribution::NonAttributable)
                .map(|d| d.to_string())
                .collect();

            let expr = if blank.is_empty() {' \
  '                .filter(|d| by_depth[*d] == Attribution::NonAttributable)
                .map(|d| d.to_string())
                .collect();

            let expr = if true {' \
  geode-data \
  grouping_by_a_carried_dimension_sums_like_the_key_it_depends_on_and_blanks_coarser_measures

# Re-anchored (2026-09-09): the bare `let expr = if blank.is_empty() {`
# occurs twice in this file -- once for a plain measure, once for a
# DERIVED column -- and `replace(..., 1)` was hitting the measure site,
# which is not what this entry's name claims to guard. The `blank` build
# above each differs (`by_depth` vs `attribution_by_depth`), so the anchor
# now carries it and lands on the derived one; the measure site is the
# entry directly above.
run_mutation "derived: the value is blanked, not just the marker" \
  crates/geode-data/src/query/compile.rs \
  '                .filter(|d| attribution_by_depth[*d] == Attribution::NonAttributable)
                .map(|d| d.to_string())
                .collect();
            let expr = if blank.is_empty() {' \
  '                .filter(|d| attribution_by_depth[*d] == Attribution::NonAttributable)
                .map(|d| d.to_string())
                .collect();
            let expr = if true {' \
  geode-data a_derived_column_is_blanked_where_its_inputs_are

run_mutation "derived: comments are stripped before scanning for columns" \
  crates/geode-data/src/query/compile.rs \
  '    let stripped = strip_sql_comments(sql);' \
  '    let stripped = sql.to_string();' \
  geode-data \
  a_comment_does_not_drag_in_columns_the_expression_never_names

run_mutation "order: tie-breakers use the spine, not the ENUM-cast alias" \
  crates/geode-data/src/query/compile.rs \
  '            order_keys.push(format!("s.\"{g}\" asc"));' \
  '            order_keys.push(format!("\"{g}\" asc"));' \
  geode-data \
  the_row_order_is_total_so_a_requery_does_not_reshuffle

run_mutation "validation: a derived dimension is a legal view column" \
  crates/geode-core/src/view.rs \
  '                    if let Some(d) = dims.get(name) {' \
  '                    if let Some(d) = None::<&crate::dimensions::DerivedDimension> {' \
  geode-core \
  a_derived_dimension_is_accepted_as_a_column_not_only_as_a_grouping

run_mutation "scope: a contradiction survives further composition" \
  crates/geode-core/src/scope/mod.rs \
  '        let mut contradicted: Vec<String> = if self.impossible {' \
  '        let mut contradicted: Vec<String> = if false {' \
  geode-core \
  a_contradiction_still_names_the_dimension_that_caused_it

run_mutation "snapshot: dimension codes report a null row as null" \
  crates/geode-core/src/snapshot.rs \
  '        if self.is_null(row) {
            return None;
        }' \
  '        if false {
            return None;
        }' \
  geode-core \
  dimension_codes_report_a_rolled_up_row_as_having_none

run_mutation "snapshot: dimensions read at UInt32 key width" \
  crates/geode-core/src/snapshot.rs \
  '    let d = arr.as_any().downcast_ref::<DictionaryArray<UInt32Type>>()?;' \
  '    let d = None::<&DictionaryArray<UInt32Type>>?;' \
  geode-core \
  a_dimension_past_the_65535_value_cliff_still_reads

run_mutation "snapshot: a summed i64 measure is readable" \
  crates/geode-core/src/snapshot.rs \
  '    if let Some(values) = arr.as_any().downcast_ref::<Decimal128Array>() {' \
  '    if let Some(values) = None::<&Decimal128Array> {' \
  geode-core \
  a_summed_integer_measure_is_readable_as_a_number

run_mutation "pool: shutdown does not deliver its own interrupt" \
  crates/geode-data/src/query/pool.rs \
  'if stale || cancelled || q.shutdown {' \
  'if stale || cancelled {' \
  geode-data \
  shutdown_does_not_deliver_its_own_interrupt_as_a_failure

run_mutation "catalog: the migration clears a crashed load's orphan id" \
  crates/geode-data/src/store/catalog.rs \
  'let start = if latest == 0 { 1 } else { latest + 2 };' \
  'let start = if latest == 0 { 1 } else { latest + 1 };' \
  geode-data \
  the_migration_skips_the_id_a_crashed_pre_sequence_load_could_hold

run_mutation "catalog: the bookless partition rolls up into the unscoped as-of" \
  crates/geode-data/src/store/catalog.rs \
  '            .filter(|(b, _)| books.is_empty() || b.as_ref().is_some_and(|b| books.contains(b)))' \
  '            .filter(|(b, _)| books.is_empty() || b.as_ref().is_none_or(|b| books.contains(b)))' \
  geode-data \
  the_bookless_partition_is_in_the_unscoped_as_of_but_not_a_named_scope

run_mutation "catalog: the backfill guard is scoped to its dataset (named book)" \
  crates/geode-data/src/store/catalog.rs \
  'where fg.dataset = ? and fg.batch = ? and fb.book = ?' \
  'where ? is not null and fg.batch = ? and fb.book = ?' \
  geode-data \
  the_backfill_guard_does_not_read_another_datasets_source_times

run_mutation "catalog: the backfill guard is scoped to its dataset (bookless)" \
  crates/geode-data/src/store/catalog.rs \
  'where fg.dataset = ? and fg.batch = ? and fb.book is null' \
  'where ? is not null and fg.batch = ? and fb.book is null' \
  geode-data \
  the_backfill_guard_does_not_read_another_datasets_source_times

run_mutation "catalog: a generation that never went live is not fresh" \
  crates/geode-data/src/store/catalog.rs \
  '                       where fg.dataset = ?
                         and coalesce(fg.archived_only, false) = false' \
  '                       where fg.dataset = ?
                         and true' \
  geode-data \
  a_generation_that_never_went_live_does_not_move_freshness

run_mutation "ingest: the publish event names the partitions written" \
  crates/geode-data/src/ingest/runner.rs \
  'books: loaded.partitions.clone(),' \
  'books: Vec::new(),' \
  geode-data \
  works_a_plan_and_reports_every_publish

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
  '    let archived_only = false && !published.is_empty()' \
  geode-data \
  an_older_file_cannot_overwrite_the_bookless_partition

run_mutation "catalog: the archived_only column is added to old catalogs" \
  crates/geode-data/src/store/catalog.rs \
  'ALTER TABLE file_generations ADD COLUMN IF NOT EXISTS archived_only BOOLEAN;' \
  '-- migration removed' \
  geode-data \
  a_catalog_written_before_archived_only_gains_the_column

# ---- the bookless partition in the catalog (spec §4.5)

run_mutation "catalog: the bookless partition gets a file_books row" \
  crates/geode-data/src/store/catalog.rs \
  '        for book in &rec.books {' \
  '        for book in rec.books.iter().filter(|b| b.is_some()) {' \
  geode-data \
  the_bookless_partition_has_freshness_of_its_own

run_mutation "catalog: freshness can be asked about a null book" \
  crates/geode-data/src/store/catalog.rs \
  'and fg.batch = ? and fb.book is null' \
  'and fg.batch = ? and fb.book is not null' \
  geode-data \
  the_backfill_guard_sees_the_bookless_partition

run_mutation "ingest: the backfill guard covers every partition written" \
  crates/geode-data/src/ingest/load.rs \
  '    for partition in &partitions {' \
  '    for partition in partitions.iter().filter(|p| p.book.is_some()) {' \
  geode-data \
  an_older_file_cannot_overwrite_the_bookless_partition

# ---- generation id allocation (spec §4.3)

run_mutation "catalog: a gen_id is reserved, not peeked" \
  crates/geode-data/src/store/catalog.rs \
  "        let sql = \"select nextval('file_generations_gen_id')\";" \
  '        let sql = "select coalesce(max(gen_id), 0) + 1 from file_generations";' \
  geode-data \
  a_reserved_gen_id_is_never_handed_out_twice

run_mutation "catalog: the gen_id sequence starts above existing generations" \
  crates/geode-data/src/store/catalog.rs \
  'let start = if latest == 0 { 1 } else { latest + 2 };' \
  'let start = 1;' \
  geode-data \
  a_database_that_predates_the_sequence_continues_above_its_generations

# ---- the grain vocabulary (spec §3.3, §6.3)

run_mutation "vocabulary: pair grain does not carry the underlying dimension" \
  crates/geode-core/src/schema/grain.rs \
  'Grain::UnderlyingPair => &K_INSTRUMENT,' \
  'Grain::UnderlyingPair => &K_PAIR,' \
  geode-data \
  cross_gamma_is_blank_below_instrument_level_and_present_above_it

run_mutation "attribution: a derived dimension resolves to its base column" \
  crates/geode-core/src/attribution.rs \
  '        .map(|c| dims.base_column(c.as_str()))' \
  '        .map(|c| c.as_str())' \
  geode-data \
  a_derived_dimension_finer_than_a_measures_grain_blanks_that_measure

run_mutation "attribution: decided on dimension keys" \
  crates/geode-core/src/attribution.rs \
  '    let key = ds.dimensions_at(grain);' \
  '    let key: Vec<&str> = grain.key_columns().to_vec();' \
  geode-data \
  cross_gamma_is_blank_below_instrument_level_and_present_above_it

# ---- scope lowering (spec §6.2, §6.3)

run_mutation "scope: a same-grain measure is direct" \
  crates/geode-data/src/query/scope_sql.rs \
  '|| ds.column(base).and_then(|c| c.grain()) == Some(grain)' \
  '|| false' \
  geode-data \
  a_measure_predicate_is_direct_not_semi_joined

run_mutation "scope: probe keys are the shared dimension keys" \
  crates/geode-data/src/query/scope_sql.rs \
  '.filter(|k| probe.dimension_key_columns().contains(k))' \
  '.filter(|k| probe.key_columns().contains(k))' \
  geode-data \
  a_pair_measure_predicate_reaches_both_underlyings_of_the_pair

run_mutation "scope: single-pass param assembly" \
  crates/geode-data/src/query/scope_sql.rs \
  'let inner_params: Vec<Value> = mine.iter().flat_map(|(_, p)| p.clone()).collect();' \
  'let inner_params: Vec<Value> = Vec::new();' \
  geode-data \
  a_finer_and_a_direct_predicate_bind_to_their_own_placeholders

run_mutation "scope: text filter reaches other grains" \
  crates/geode-data/src/query/scope_sql.rs \
  'terms.push(membership(ds, grain, probe, era, &test));' \
  '{ let _ = probe; continue; }' \
  geode-data \
  a_text_filter_reaches_coarse_measures_by_membership

run_mutation "scope: LIKE wildcards escaped" \
  crates/geode-data/src/query/scope_sql.rs \
  'if matches!(ch,' \
  'if false && matches!(ch,' \
  geode-data \
  the_text_filter_escapes_likes_own_wildcards

run_mutation "scope: conjuncts routed separately" \
  crates/geode-data/src/query/scope_sql.rs \
  'for term in conjuncts(expr) {' \
  'for term in [expr] {' \
  geode-data \
  conjuncts_of_different_grains_route_separately

# ---- the compiler (spec §6.3, §6.4, §6.8)

run_mutation "spine: assembled from every aggregate, not the finest" \
  crates/geode-data/src/query/compile.rs \
  '.filter(|d| own_present[*d] == *d).collect();' \
  '.filter(|d| own_present[*d] == *d && own.len() == depth).collect();' \
  geode-data \
  a_cash_only_position_has_a_row_at_the_lhu_level

run_mutation "spine: the grand total row is constant" \
  crates/geode-data/src/query/compile.rs \
  'spine_sources.join(" union all ")' \
  'spine_sources[1..].join(" union all ")' \
  geode-data \
  a_scope_selecting_nothing_still_yields_the_grand_total_row

# The mutation groups the reference side one row per source row instead of
# one row per join key, which is exactly what "aggregate to the join key"
# forbids: two reference rows for one instrument then duplicate every spine
# row they match. `random()` is the defeat because no deterministic
# expression here can be finer than the key — a function of `{keys}` is
# only ever coarser, and the relation is sometimes a subquery, so `rowid`
# does not bind. Simply deleting `group by {keys}` is NOT the mutation to
# write: the projection holds `any_value(...)` beside the bare key columns,
# so DuckDB raises a binder error and the entry reports "caught" for a
# failure that says nothing about the aggregation. It also left `keys`
# unused inside the `format!`, so the crate did not compile and the entry
# was printing "caught" for a build failure — CLAUDE.md's "no test behind
# it" lie, found in the 2026-09-10 filter sweep. Keep the filter: with it
# the entry takes ~3s, while the full-suite fallback under this mutation
# takes ~15 minutes (the multiplied reference rows blow up elsewhere in the
# suite) against ~36s for an ordinary geode-data entry.
run_mutation "join: aggregate to the join key" \
  crates/geode-data/src/query/compile.rs \
  'group by {keys}) {alias} on {on}' \
  'group by {keys}, random()) {alias} on {on}' \
  geode-data \
  a_reference_row_per_holder_does_not_multiply_the_spine

run_mutation "aggregate: sub_depth level guard" \
  crates/geode-data/src/query/compile.rs \
  '.chain(std::iter::once(level))' \
  '' \
  geode-data \
  a_real_null_in_a_grouping_column_does_not_fan_out_the_tree

run_mutation "derived: scalar projection" \
  crates/geode-data/src/query/compile.rs \
  '    if derived.is_empty() {' \
  '    if true {' \
  geode-data \
  grouping_by_a_derived_dimension_produces_its_mapped_values

# ---- publish (spec §4.3, §4.4)

run_mutation "publish: the bookless partition is replaced" \
  crates/geode-data/src/store/publish.rs \
  'None => "book is null".to_string(),' \
  'None => "book = %".to_string(),' \
  geode-data \
  the_bookless_partition_is_replaced_like_any_other

run_mutation "ingest: the bookless partition is published" \
  crates/geode-data/src/ingest/load.rs \
  '.chain((unattributed_rows > 0).then_some(None))' \
  '.chain(None)' \
  geode-data \
  republishing_a_file_with_bookless_rows_does_not_accumulate_them

# ---- the query pool (spec §6.7, §7.3, §10.1)

run_mutation "pool: a panicking query does not wedge its view" \
  crates/geode-data/src/query/pool.rs \
  '        let outcome = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            geode_core::panic::contained(|| run(&conn, &req))
        })) {
            Ok(r) => r.map_err(|e| e.to_string()),
            Err(payload) => Err(panic_message(&*payload)),
        };' \
  '        let outcome = run(&conn, &req).map_err(|e| e.to_string());' \
  geode-data \
  a_panicking_query_degrades_its_view_without_wedging_it

run_mutation "pool: a post-shutdown submit is not queued" \
  crates/geode-data/src/query/pool.rs \
  '        if q.shutdown {
            return id;
        }' \
  '        if false {
            return id;
        }' \
  geode-data \
  a_post_shutdown_submit_is_not_queued

run_mutation "pool: a cancelled query delivers nothing, not an error" \
  crates/geode-data/src/query/pool.rs \
  'let cancelled = q.cancelled.remove(&id);' \
  'let cancelled = { q.cancelled.remove(&id); false };' \
  geode-data \
  cancelling_a_running_query_delivers_no_failure

# No entry for allocating the query id under the lock, nor for holding the
# lock across the stale check and the send. Both are races: the mutation is
# only observable on an interleaving the test cannot force, so an entry
# would report SURVIVED whether the code is right or wrong. They are
# argued in comments at the site instead.

# ---- row order (spec §6.3; the blotter's flatten walk)

run_mutation "order: emitted even when the view declares no sort" \
  crates/geode-data/src/query/compile.rs \
  'let order = format!(" order by {}", order_keys.join(", "));' \
  'let order = if view.sort.is_empty() { String::new() } else { format!(" order by {}", order_keys.join(", ")) };' \
  geode-data \
  rows_arrive_shallowest_first_when_the_view_declares_no_sort

run_mutation "order: shallowest first" \
  crates/geode-data/src/query/compile.rs \
  'let mut order_keys = vec!["s.row_depth asc".to_string()];' \
  'let mut order_keys: Vec<String> = Vec::new();' \
  geode-data \
  rows_arrive_shallowest_first_when_the_view_declares_no_sort

run_mutation "order: grouping columns break ties" \
  crates/geode-data/src/query/compile.rs \
  '    for g in view.grouping.iter().take(depth) {' \
  '    for g in view.grouping.iter().take(0) {' \
  geode-data \
  the_row_order_is_total_so_a_requery_does_not_reshuffle

# ---- the snapshot read path (spec §6.6, §6.3)

# A MEASURE, so the site is `f64_in`'"'"'s Float64 arm; `str_in` ends in the
# same expression verbatim, and the `return ` prefix is what tells them
# apart at the nearest possible distance.
run_mutation "snapshot: a null measure is not zero" \
  crates/geode-core/src/snapshot.rs \
  '        return (row < values.len() && !values.is_null(row)).then(|| values.value(row));' \
  '        return (row < values.len()).then(|| values.value(row));' \
  geode-core \
  a_null_measure_reads_as_none_not_zero

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
  geode-core \
  depth_is_read_at_whatever_integer_width_it_arrives_in

run_mutation "snapshot: depth reads at DuckDB's own width, end to end" \
  crates/geode-core/src/snapshot.rs \
  '        let depth = self.i64_at(self.depth_col?, row)?;' \
  '        let depth = *self.i64_column("row_depth")?.get(row)?;' \
  geode-data \
  a_blanked_cross_gamma_is_still_blank_after_the_snapshot_boundary

run_mutation "snapshot: a rolled-up dimension cell is null" \
  crates/geode-core/src/snapshot.rs \
  'if row >= d.len() || d.is_null(row) {' \
  'if row >= d.len() {' \
  geode-core \
  a_rolled_up_dimension_cell_is_none_not_the_first_dictionary_entry

run_mutation "snapshot: dimension cells read at UInt16 key width" \
  crates/geode-core/src/snapshot.rs \
  '    if let Some(d) = arr.as_any().downcast_ref::<DictionaryArray<UInt16Type>>() {
        return dictionary_cell(d, row);
    }' \
  '    if let Some(d) = None::<&DictionaryArray<UInt16Type>> {
        return dictionary_cell(d, row);
    }' \
  geode-core \
  a_dimension_past_the_255_value_cliff_reads_at_either_width

run_mutation "snapshot: dictionary columns expose UInt16 codes" \
  crates/geode-core/src/snapshot.rs \
  '        return Some((DictCodes::U16(d.keys().values(), d.nulls()), values));' \
  '        return None;' \
  geode-core \
  a_dimension_past_the_255_value_cliff_reads_at_either_width

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
  geode-core \
  dictionary_batches_concatenate_at_either_key_width

run_mutation "snapshot: text reads a dimension under either era encoding" \
  crates/geode-core/src/snapshot.rs \
  '    dict_cell_in(arr, row).or_else(|| str_in(arr, row))' \
  '    str_in(arr, row)' \
  geode-core \
  a_dimension_reads_the_same_under_either_era_encoding

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

# The Query arm, which is what `an_outcome_is_addressed_to_the_key_that_asked`
# reads; the Distinct arm right below copies the same field verbatim and is
# covered by `a_distinct_query_returns_value_counts_on_the_distinct_event`.
run_mutation "service: an outcome carries the caller's key" \
  crates/geode-data/src/service.rs \
  '                RequestKind::Query => sink(DataEvent::Query(QueryOutcome {
                    key: r.key,' \
  '                RequestKind::Query => sink(DataEvent::Query(QueryOutcome {
                    key: QueryKey(0),' \
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
                source: item.source.clone(),
                dataset: item.dataset.clone(),
                batch: item.batch.clone(),
                reason: format!("dataset '"'"'{}'"'"' is not declared", item.dataset),
            });
            clear_in_flight(&queue);
            if !failed {
                log_refused_event(
                    &refusal_logged,
                    &format!(
                        "the undeclared-dataset failure for {}/{}",
                        item.dataset, item.batch
                    ),
                );
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
  '                    if ready > 0 {
                        ingest.submit(plan);
                    }' \
  '                    let _ = plan;' \
  geode-data \
  a_file_that_appears_after_start_is_discovered_and_published

run_mutation "scheduler: pending-too-long surfaces as health" \
  crates/geode-data/src/ingest/scheduler.rs \
  '            CandidateState::PendingTooLong => Health::PendingTooLong,' \
  '            CandidateState::PendingTooLong => continue,' \
  geode-data \
  a_csv_pending_past_its_timeout_is_a_health_event

# Re-anchored, Phase 4b Task 2 fix round 1: the Published arm grew an
# `info!` call and a `rows` binding around the same `sink(...)` call this
# entry has always been about; the anchor now targets just that inner
# call so it's independent of the tracing addition (which has its own
# coverage — geode-data's own `service::tests` module).
run_mutation "service: a publish becomes a Published event" \
  crates/geode-data/src/service.rs \
  '                    let delivered = sink(DataEvent::Published {
                        dataset,
                        batch: batch.clone(),
                        gen_id,
                        books,
                    });' \
  '                    let delivered = true;
                    let _ = (dataset, gen_id, books);' \
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

# §17.1 rule 2: one click captures. Mutated back to the two-click rule,
# the first click on an unselected row only selects.
run_mutation "keybindings: a single row click starts listening" \
  crates/geode-shell/src/shell/keybindings_view.rs \
  '    state.selected = clicked_ix;
    state.listening = Some(Vec::new());' \
  '    if state.selected == clicked_ix && state.listening.is_none() {
        state.listening = Some(Vec::new());
    } else {
        state.selected = clicked_ix;
        state.listening = None;
    }' \
  geode-shell \
  a_single_click_on_a_row_starts_listening

# ---- grouping slots and the frame (Phase 3 §4)

run_mutation "groupings: an unknown column drops the slot" \
  crates/geode-core/src/groupings.rs \
  '            if let Some(unknown) = grouping.iter().find(|c| !known(c)) {' \
  '            if let Some(unknown) = grouping.iter().find(|c| !known(c) && false) {' \
  geode-core \
  an_unknown_column_is_an_error_for_that_slot_only

# The grouping vocabulary is the compiler's (`carries_all`): a column some
# DECLARED grain carries, spelled out once in `DatasetSpec::groupable_columns`
# for the Groupings dialog and the blotter's `:group` completion. Dropping
# the grain test offers every declared column — a measure, a categorical
# attribute — and a slot naming one would fail to compile.
run_mutation "groupable_columns: only a column some declared grain carries is groupable" \
  crates/geode-core/src/schema/mod.rs \
  '            .filter(|c| grains.iter().any(|g| self.carries(*g, &c.name)))' \
  '            .filter(|_| true)' \
  geode-core \
  groupable_columns_are_the_declared_columns_some_declared_grain_carries

# A derived dimension is only groupable over a groupable base column: the
# compiler resolves it through `dims.base_column` and refuses one over an
# attribute no grain carries.
run_mutation "groupable_columns: a derived dimension over an ungroupable column is not offered" \
  crates/geode-shell/src/shell/mod.rs \
  '        .filter(|dim| base_groupable(&dim.from))' \
  '        .filter(|_| true)' \
  geode-shell \
  groupable_columns_are_every_carried_dimension_key_included_plus_derived

# A non-interned grouping column is selected as VARCHAR whatever its
# storage type — the snapshot reads a grouping column as text and nothing
# else, so a raw DOUBLE strike level would paint blank.
run_mutation "compile: a non-interned grouping column is selected as varchar" \
  crates/geode-data/src/query/compile.rs \
  '            selects.push(format!("s.\"{g}\"::{ty} as \"{g}\""));' \
  '            selects.push(format!("s.\"{g}\" as \"{g}\""));' \
  geode-data \
  grouping_by_a_numeric_carried_dimension_produces_a_level_with_its_values

# The dialog must read the grouping vocabulary, not the picker's: the
# picker's list has no key columns (`position_ref`) and no numeric
# dimension, and offers attributes no grain can group by.
run_mutation "groupings dialog: fields offers groupable_columns, not pickable_columns" \
  crates/geode-shell/src/shell/objectdialog/groupings.rs \
  '    for column in crate::shell::groupable_columns(config) {' \
  '    for column in crate::shell::pickable_columns(config) {' \
  geode-shell \
  fields_offers_every_groupable_column_chain_first_then_the_rest

# Only a utf8 column can be an ENUM; a dimension's categorical DEFAULT is
# gated on the type, or an f64 dimension is sent for interning.
run_mutation "schema: a non-string dimension does not default to categorical" \
  crates/geode-core/src/schema/mod.rs \
  '        matches!(role, ColumnRole::Dimension { .. }) && ty == ColumnType::Utf8;' \
  '        matches!(role, ColumnRole::Dimension { .. });' \
  geode-core \
  a_non_string_dimension_defaults_to_uncategorical_without_a_diagnostic

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

# ---- chords from the scope bar's text field (Phase 4 §3.11, ruling 2026-09-12)

run_mutation "field chords: a modified keystroke in the focused field dispatches its shell binding" \
  crates/geode-shell/src/shell/input.rs \
  '                && ks.mods.is_chord()' \
  '                && false' \
  geode-shell \
  a_chord_typed_into_the_focused_field_dispatches_and_a_shifted_letter_types

run_mutation "field chords: shift alone is typing, not a chord" \
  crates/geode-shell/src/keymap/keystroke.rs \
  '        self.ctrl || self.alt || self.cmd' \
  '        self.ctrl || self.alt || self.cmd || self.shift' \
  geode-shell \
  a_chord_typed_into_the_focused_field_dispatches_and_a_shifted_letter_types

run_mutation "field chords: resolved against the workspace context alone, never the focused tile's" \
  crates/geode-shell/src/shell/input.rs \
  '                let stack = [KeyContext::new("workspace")];' \
  '                let stack = self.context_stack(cx);' \
  geode-shell \
  a_chord_in_the_focused_tiles_own_context_does_not_fire_from_the_field

run_mutation "field chords: an unbind swallows the chord rather than falling through to the builtin" \
  crates/geode-shell/src/shell/input.rs \
  '                    if action.0 != UNBOUND_ACTION {' \
  '                    if true {' \
  geode-shell \
  an_unbound_chord_typed_into_the_field_is_swallowed

run_mutation "field chords: the last matching layer wins, so a user unbind shadows the builtin" \
  crates/geode-shell/src/shell/input.rs \
  '        self.services.keymap.bindings().iter().rfind(|binding| {' \
  '        self.services.keymap.bindings().iter().find(|binding| {' \
  geode-shell \
  an_unbound_chord_typed_into_the_field_is_swallowed

run_mutation "field chords: a dispatched chord reflects the frame's text back into the still-focused field" \
  crates/geode-shell/src/shell/input.rs \
  '                        self.reflect_frame_text_into_focused_field(window, cx);' \
  '' \
  geode-shell \
  a_scope_undo_chord_from_the_field_reflects_the_frames_text_into_it

# ---- overlays return focus to the field they opened from (ruling 2026-09-12)

run_mutation "overlay focus: the dialog door records whether the field held focus" \
  crates/geode-shell/src/shell/dialog.rs \
  '    view.overlay_return_to_filter = view.filter_field_focused(window, cx);' \
  '    view.overlay_return_to_filter = false;' \
  geode-shell \
  a_dialog_opened_from_the_field_returns_focus_to_it_when_closed

run_mutation "overlay focus: closing an overlay returns focus to the field when recorded" \
  crates/geode-shell/src/shell/mod.rs \
  '        if std::mem::take(&mut self.overlay_return_to_filter) {' \
  '        if !std::mem::take(&mut self.overlay_return_to_filter) && false {' \
  geode-shell \
  a_dialog_opened_from_the_field_returns_focus_to_it_when_closed

run_mutation "overlay focus: the palette's open arm records whether the field held focus" \
  crates/geode-shell/src/shell/palette_ctl.rs \
  '        self.overlay_return_to_filter = self.filter_field_focused(window, cx);' \
  '        self.overlay_return_to_filter = false;' \
  geode-shell \
  the_palette_opened_from_the_field_returns_focus_to_it_on_escape

run_mutation "overlay focus: closing a palette that was not open leaves focus alone" \
  crates/geode-shell/src/shell/palette_ctl.rs \
  '        if self.palette.take().is_none() {' \
  '        if self.palette.take().is_none() && false {' \
  geode-shell \
  a_dialog_opened_from_the_field_returns_focus_to_it_when_closed

# Re-anchored 2026-09-08 (add-tile): the bare
# `o.content.set_visible(false, cx);` line matched the FIRST of two
# occurrences — MAJ-2's vanished-occupant loop, which the MAJ-2 entry
# below already owns — so this entry never reached the visibility diff
# its name and its test are about, and read `caught*`. The whole diff
# loop is the anchor now.
run_mutation "hosting: leaving the screen is announced" \
  crates/geode-shell/src/shell/occupants.rs \
  '        for id in self.visible_tiles.difference(&active) {
            any_tile_left_the_screen = true;
            if let Some(o) = self.occupants.get(id) {
                o.content.set_visible(false, cx);
            }
        }' \
  '        for id in self.visible_tiles.difference(&active) {
            any_tile_left_the_screen = true;
            let _ = id;
        }' \
  geode-shell \
  closing_a_tile_drops_its_occupant_and_switching_workspaces_toggles_visibility

# The behaviour the entry above used to mutate by accident, now named in
# its own right (Phase 4b Task 5 fix round 1, MAJ-2): a tile that vanished
# between renders is told it is invisible BEFORE its occupant is dropped,
# so an occupant that opened something in `set_visible(true)` gets the
# matching close rather than leaking it for the life of the process. The
# visibility diff further down cannot cover this — a closed tile is not in
# `active`, so that diff never sees it. Narrower than the `shell: MAJ-2 —
# ensure_occupants ...` entry much further down, which deletes the whole
# loop: this one keeps the loop and removes only the announcement.
run_mutation "hosting: a vanished tile is told before its occupant is dropped" \
  crates/geode-shell/src/shell/occupants.rs \
  '        for (id, o) in self.occupants.iter() {
            if !all.contains(id) {
                o.content.set_visible(false, cx);' \
  '        for (id, o) in self.occupants.iter() {
            if !all.contains(id) {
                let _ = o;' \
  geode-shell \
  closing_a_watching_tile_unwatches_the_diagnostics_entity

# Rewritten 2026-09-08 (add-tile): there is no fallback factory any more
# (spec 2026-09-08 add-tile §7.1), so `matched.and(restored)` is no
# longer the guard — the `match (matched, pending_factory)` below only
# reads `restored_state` in the arm where `matched` is `Some`, which
# makes the old mutation a no-op. The live behaviour is one step
# earlier: a restored record matches a factory of its OWN kind, or none.
run_mutation "hosting: a restored record only ever matches a factory of its own kind" \
  crates/geode-shell/src/shell/occupants.rs \
  '            let matched = restored
                .as_ref()
                .and_then(|r| self.services.roster.factory(&r.kind));' \
  '            let matched = restored.as_ref().and_then(|_| {
                self.services
                    .roster
                    .kinds()
                    .first()
                    .copied()
                    .and_then(|k| self.services.roster.factory(k))
            });' \
  geode-shell \
  a_restored_tile_of_an_unknown_kind_paints_the_placeholder_and_its_record_survives

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
  '        if !self.session_dirty && !frame_dirty && !usage_dirty && tiles == self.last_tiles_written {' \
  '        if !self.session_dirty && !frame_dirty && !usage_dirty {' \
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

run_mutation "tile chrome: the unfocused tile pads the pixel its thinner ring gives up, so focus never shifts content" \
  crates/geode-shell/src/shell/render.rs \
  '                .when(!is_focused, |el| el.border_1().p(px(1.0)))' \
  '                .when(!is_focused, |el| el.border_1())' \
  geode-shell \
  moving_focus_does_not_shift_tile_content

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

# Re-anchored (Phase 4b Task 4 fix round 2, ruling 4 / fix round 1's
# own re-anchor attempt, which never made it into the committed
# script): the code moved from a direct `self.restart_required = None;`
# assignment to an `if restart.is_empty() { None } else { Some(..) }`
# expression assigned once via `restart_message.clone()`. Mutated to
# only assign when `Some`, reintroducing "never clears".
run_mutation "frame: restart_required clears once sources/datasets match the baseline again (M8)" \
  crates/geode-shell/src/shell/hot_reload.rs \
  '            self.restart_required = restart_message.clone();' \
  '            if restart_message.is_some() {
                self.restart_required = restart_message.clone();
            }' \
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
' \
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
            }
' \
  geode-shell \
  emits_config_reloaded_before_the_frame_notifies

run_mutation "frame: bar_model is rebuilt when versions change" \
  crates/geode-shell/src/frame.rs \
  '        if let Some((cached_versions, cached_today, cached)) = self.bar_cache.borrow().as_ref()
            && *cached_versions == versions
            && *cached_today == today
        {
            return Rc::clone(cached);
        }' \
  '        if let Some((cached_versions, cached_today, cached)) = self.bar_cache.borrow().as_ref()
            && *cached_versions != versions
            && *cached_today == today
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
  '                (Some("desc"), None, _) => SortOrder::Desc,' \
  '                (Some("desc"), None, _) => SortOrder::Asc,' \
  geode-blotter \
  every_command_parses

run_mutation "commands: a bare sort abs is abs desc" \
  crates/geode-blotter/src/core/commands.rs \
  '                (Some("abs"), None, _) | (Some("abs"), Some("desc"), None) => SortOrder::AbsDesc,' \
  '                (Some("abs"), None, _) | (Some("abs"), Some("desc"), None) => SortOrder::AbsAsc,' \
  geode-blotter \
  every_command_parses

run_mutation "flatten: the absolute orders compare magnitudes, not signed values" \
  crates/geode-blotter/src/core/flatten.rs \
  '    let key = |v: f64| if absolute { v.abs() } else { v };' \
  '    let key = |v: f64| v;' \
  geode-blotter \
  absolute_orders_compare_magnitudes_and_keep_null_last

run_mutation "flatten: NaN is a NULL to the comparator, not Equal-to-everything" \
  crates/geode-blotter/src/core/flatten.rs \
  '    snapshot.f64_at(i, row).filter(|v| !v.is_nan())' \
  '    snapshot.f64_at(i, row)' \
  geode-blotter \
  nan_sorts_last_like_null_and_never_panics_the_sort

run_mutation "sort cycle: S is inert on a column with no magnitude" \
  crates/geode-blotter/src/core/flatten.rs \
  '        if absolute && !measure {' \
  '        if false {' \
  geode-blotter \
  shift_s_is_inert_on_a_column_with_no_magnitude_where_s_is_not

run_mutation "sort cycle: S steps abs desc on to abs asc before clearing" \
  crates/geode-blotter/src/core/flatten.rs \
  '                Some(SortOrder::AbsDesc) => Some(SortOrder::AbsAsc),' \
  '                Some(SortOrder::AbsDesc) => None,' \
  geode-blotter \
  the_key_cycles_walk_their_own_orders_and_restart_from_the_others

run_mutation "sort cycle: s from an absolute order restarts at asc, not desc" \
  crates/geode-blotter/src/core/flatten.rs \
  '                _ => Some(SortOrder::Asc),' \
  '                _ => Some(SortOrder::Desc),' \
  geode-blotter \
  the_key_cycles_walk_their_own_orders_and_restart_from_the_others

run_mutation "click cycle: a measure's click goes on from asc to abs desc, not to cleared" \
  crates/geode-blotter/src/core/flatten.rs \
  '            Some(SortOrder::Asc) if measure => Some(SortOrder::AbsDesc),' \
  '            Some(SortOrder::Asc) if measure => None,' \
  geode-blotter \
  a_header_click_walks_every_order_a_measure_can_show_desc_first

run_mutation "click cycle: a text column's click clears after asc instead of entering the absolute pair" \
  crates/geode-blotter/src/core/flatten.rs \
  '            Some(SortOrder::Asc) if measure => Some(SortOrder::AbsDesc),' \
  '            Some(SortOrder::Asc) => Some(SortOrder::AbsDesc),' \
  geode-blotter \
  a_header_click_on_a_text_column_skips_the_absolute_pair

run_mutation "delegate: a header-click resort moves the component's highlight with the cursor" \
  crates/geode-blotter/src/delegate.rs \
  '            table.set_selected_row(row, cx);' \
  '            let _ = row;' \
  geode-blotter \
  a_header_click_cycles_through_the_absolute_orders_too

run_mutation "delegate: a header click steps the blotter's own cycle, not the component's proposal" \
  crates/geode-blotter/src/delegate.rs \
  '        let next = SortOrder::click_cycle(current, self.is_measure(col_ix));' \
  '        let next = SortOrder::cycle(current, false, self.is_measure(col_ix));' \
  geode-blotter \
  a_header_click_cycles_through_the_absolute_orders_too

run_mutation "sort order: abs asked of a text column is its signed direction" \
  crates/geode-blotter/src/core/flatten.rs \
  '            (SortOrder::AbsDesc, false) => SortOrder::Desc,' \
  '            (SortOrder::AbsDesc, false) => SortOrder::AbsDesc,' \
  geode-blotter \
  an_absolute_order_asked_of_a_text_column_becomes_its_signed_direction

run_mutation "tile: the key cycle reaches the delegate through SortOrder::cycle, not a stale copy" \
  crates/geode-blotter/src/tile.rs \
  '                    let next = SortOrder::cycle(current, absolute, measure);' \
  '                    let next = SortOrder::cycle(current, false, measure);' \
  geode-blotter \
  s_and_shift_s_cycle_signed_and_absolute_sorts

run_mutation "delegate: the header marks an absolute sort" \
  crates/geode-blotter/src/delegate.rs \
  '        if own_sort.is_some_and(|s| s.order.absolute()) {' \
  '        if false {' \
  geode-blotter \
  s_and_shift_s_cycle_signed_and_absolute_sorts

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

# `Command::FilterExpr`, which is the arm the named test drives
# (`filter model_code = 'EURP'`); `Command::FilterText` right below it
# assigns and requeries identically.
run_mutation "blotter: :filter narrows only this tile" \
  crates/geode-blotter/src/tile.rs \
  '                self.validate_tile_scope(&scope)?;
                self.tile_scope = scope;
                self.requery(cx);' \
  '                self.validate_tile_scope(&scope)?;
                self.requery(cx);' \
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
  '                    Some(ds) => ds
                        .groupable_columns()
                        .into_iter()
                        .map(str::to_string)
                        .collect(),' \
  '                    Some(_) => Vec::new(),' \
  geode-blotter \
  completions_offer_dataset_dimensions_not_just_displayed_columns

# ---- blotter mouse toggles (2026-09-12): a row double-click and a chevron
# click are `space`. The chevron listener's explicit cursor move in
# `toggle_row` is deliberately NOT an entry: the table's own `SelectRow`
# also lands the cursor there, so the two defences overlap and neither
# is isolated (header rule 2).

run_mutation "tile: a row double-click toggles the row" \
  crates/geode-blotter/src/tile.rs \
  '            TableEvent::DoubleClickedRow(row) => this.toggle_row(*row, cx),' \
  '            TableEvent::DoubleClickedRow(_) => {}' \
  geode-blotter \
  a_double_click_on_a_row_toggles_it_like_space

run_mutation "delegate: a single chevron click toggles, a second press is ignored" \
  crates/geode-blotter/src/delegate.rs \
  '                        if e.click_count() > 1 {' \
  '                        if e.click_count() > 0 {' \
  geode-blotter \
  a_chevron_click_toggles_the_row

run_mutation "delegate: the chevron stops the row's own click from double-toggling" \
  crates/geode-blotter/src/delegate.rs \
  '                        cx.stop_propagation();' \
  '                        let _ = 0;' \
  geode-blotter \
  a_double_click_on_the_chevron_toggles_once

# ---- geode-app: the data bridge, the roster, --demo (Phase 3 §5.1, §5.4, §7.1)

run_mutation "bridge: dropped_events counted on a refused try_send" \
  crates/geode-app/src/bridge.rs \
  '            dropped.fetch_add(1, Ordering::Relaxed);
            if err.is_closed()' \
  '            if err.is_closed()' \
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
#
# Phase 4b Task 4 fix round 1: re-anchored again — `ShellView::
# set_data_status` (what the replacement text called) no longer exists,
# deleted by Task 4 in favour of the `Diagnostics` entity. The
# replacement now routes the `Health` arm through `shell_direct`'s own
# `diagnostics().update(..)` (a real, still-live call: `note_health`),
# keeping the exact same shape — bypass the shared `window.update`, do
# real work through a window-independent clone, `continue` before the
# window check ever runs.
run_mutation "bridge: every event branch, not just Query, ends the drain task on a closed window" \
  crates/geode-app/src/bridge.rs \
  '    cx.spawn(async move |cx: &mut AsyncApp| {
        let diagnostics = diagnostics_for_drain;
        let catalog_tag = catalog_tag_for_drain;
        let mut last_dropped = 0u64;
        while let Ok(event) = rx.recv().await {
            let now_dropped = dropped.load(Ordering::Relaxed);' \
  '    let shell_direct = shell.clone();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let diagnostics = diagnostics_for_drain;
        let catalog_tag = catalog_tag_for_drain;
        let mut last_dropped = 0u64;
        while let Ok(event) = rx.recv().await {
            let now_dropped = dropped.load(Ordering::Relaxed);
            if let DataEvent::Health { source, worst, detail } = &event {
                shell_direct.update(cx, |s, cx| {
                    s.diagnostics().update(cx, |d, cx| {
                        d.note_health(source, worst.clone(), detail.clone(), std::time::SystemTime::now());
                        cx.notify();
                    });
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
  '        matches!(role, ColumnRole::Dimension { .. }) && ty == ColumnType::Utf8;' \
  '        true;' \
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
  '    for c in &mut ds.columns {
        if unroutable.contains(&c.name) {
            c.textual = false;
        }
    }

    diags
}' \
  '    for c in &mut ds.columns {
        if false {
            c.textual = false;
        }
    }

    diags
}' \
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

# `push_undo`, not `clear_history` -- both end in the same line, and only
# the former is "a new set clears redo". `clear_history` is the restored-
# session path and clears both stacks deliberately.
run_mutation "frame: a new set clears redo" \
  crates/geode-shell/src/frame.rs \
  '            self.scope_undo.remove(0);
        }
        self.scope_redo.clear();' \
  '            self.scope_undo.remove(0);
        }
        let _ = &self.scope_redo;' \
  geode-shell undo_and_redo_walk_a_bounded_stack

run_mutation "frame: a text session pushes once" \
  crates/geode-shell/src/frame.rs \
  '            Some(_) => {}' \
  '            Some(_) => {
                let outgoing = self.scope.clone();
                self.push_undo(outgoing);
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

# 2026-09-12 fuzzy alignment: the matcher scores every placement and
# returns the best, so `ling` over "add tile tiling" lands on the one run
# in "tiling" rather than the greedy `l` of "tile" plus scattered hits.
# Dropping the "continue the run" candidate leaves every placement scoring
# base only, and the leftmost tie-break paints exactly the old greedy
# highlight — the defect the screenshot of 2026-09-12 showed.
run_mutation "palette: the alignment continues a run rather than restarting greedily" \
  crates/geode-shell/src/palette.rs \
  '                    let cont = ends_at[prev].map(|s| s + run_at(j));' \
  '                    let cont: Option<u32> = None;' \
  geode-shell indices_prefer_a_later_contiguous_run_over_an_earlier_scattered_one

# Same seam, the other half: the run bonus must outbid a word start, or
# `app` on "Apple Pie" paints `Ap` + `P`. Mutating the comparison to weigh
# a restart above a continuation (the tie-break reversed) reaches it
# without tripping the compile-time `RUN_BONUS > WORD_START_BONUS` assert.
run_mutation "palette: extending a run outbids restarting at a word start" \
  crates/geode-shell/src/palette.rs \
  '                    cell = fresh.max(cont).map(|s| s + base);' \
  '                    cell = fresh.or(cont).map(|s| s + base);' \
  geode-shell indices_are_contiguous_for_a_prefix_match

run_mutation "palette: selecting a saved scope loads it" \
  crates/geode-shell/src/shell/palette_ctl.rs \
  '                    if let Ok(true) = f.load_scope(&name) {' \
  '                    if let Ok(true) = f.load_scope("no-such-scope") {' \
  geode-shell a_saved_scope_appears_in_the_palette_and_selecting_it_loads_it

# 2026-09-12 palette ranking: the category joins the match text at a
# discount. Without the discount a word-start run in a category (`work`
# in "Workspace", 39) outbids the same letters mid-word in a title
# ("Framework tools", 31), and the category stops being the
# lower-preference field the design names it.
run_mutation "palette: a category character earns less than a title character" \
  crates/geode-shell/src/palette.rs \
  '        score.div_ceil(CATEGORY_DIVISOR)' \
  '        score' \
  geode-shell a_mid_word_title_match_outranks_a_word_start_category_match

# The usage bonus must actually reach the sort key — dropping it leaves
# an empty query in bare registry order, the pre-feature behaviour.
run_mutation "palette: the usage bonus is added to the match score" \
  crates/geode-shell/src/palette.rs \
  '                scored.push((i, score + self.bonus[i], indices));' \
  '                scored.push((i, score, indices));' \
  geode-shell an_empty_query_lists_used_items_first_by_bonus_then_registry_order

# Recency must be bucketed by age, not flat: reading every record as
# just-used leaves frequency alone to rank, and a command used once this
# minute no longer outranks one used once a month ago.
run_mutation "palette usage: the recency bonus falls with age" \
  crates/geode-shell/src/palette_usage.rs \
  '        RECENCY_BONUS[bucket] + self.count.saturating_sub(1).min(FREQUENCY_CAP)' \
  '        RECENCY_BONUS[0] + self.count.saturating_sub(1).min(FREQUENCY_CAP)' \
  geode-shell a_more_recent_use_earns_more_than_an_older_one_at_equal_count

# The map is bounded: without the prune the session file grows with every
# theme or scope a trader ever cycles through.
run_mutation "palette usage: recording past the cap prunes the lowest entry" \
  crates/geode-shell/src/palette_usage.rs \
  '        while self.entries.len() > MAX_ENTRIES {' \
  '        while self.entries.len() > usize::MAX {' \
  geode-shell recording_past_the_cap_drops_the_lowest_bonus_entry

# `palette::toggle`'s own row only closes the palette; counting it as a
# use would climb it up the ranking for doing nothing.
run_mutation "palette: the toggle row records no use" \
  crates/geode-shell/src/shell/palette_ctl.rs \
  '        if !is_toggle {' \
  '        {' \
  geode-shell choosing_the_palette_toggle_row_records_nothing

# The backtrack must use the same discounted constants as the forward
# pass. Drifting it back to the plain RUN_BONUS keeps every SCORE right
# and corrupts only the INDICES — `ic` on "Perf overlay" / "Diagnostics"
# paints the title's `i` plus the category's `c` instead of the one run —
# the indices-wrong-score-right hole a marker-only test cannot see.
run_mutation "palette: the backtrack continues a run by the discounted bonus" \
  crates/geode-shell/src/palette.rs \
  '            Some(s) if s + run_at(j) + base == cell => j - 1,' \
  '            Some(s) if s + RUN_BONUS + base == cell => j - 1,' \
  geode-shell a_contiguous_category_match_backtracks_to_one_run

# A palette dispatch that mutates nothing else (a theme row) must still
# reach the session flush, or a restart forgets every use since the last
# layout change.
run_mutation "palette usage: a usage change alone dirties the session flush" \
  crates/geode-shell/src/shell/session_io.rs \
  '        if !self.session_dirty && !frame_dirty && !usage_dirty && tiles == self.last_tiles_written {' \
  '        if !self.session_dirty && !frame_dirty && tiles == self.last_tiles_written {' \
  geode-shell a_palette_dispatch_reaches_the_session_flush

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

# ---- Phase 4c Task 3: view presentation (spec §5.6, §4.1) -------------

# The whole design rests on ORDER: `Config::load` merges the named
# objects, and only then is `view_presentation` merged over the views.
# Drop that second half and every reader still gets perfectly valid
# views — the desk's, with the trader's column order, hidden set and
# widths silently gone. Nothing errors; the personal file simply stops
# existing. Two entries because there are two doors and each can be
# reverted independently: the core loader itself, and the one production
# caller that must go through it rather than reading the `views` doc.
run_mutation "views: the user's presentation is merged over the view in the loader" \
  crates/geode-core/src/config/load.rs \
  '    if let Some(doc) = config.doc("view_presentation") {' \
  '    if let Some(doc) = None::<&crate::config::MergedDoc> {' \
  geode-core \
  presentation_is_merged_over_the_view_after_the_named_object_merge

run_mutation "views: data_setup goes through load_views, not the raw views doc" \
  crates/geode-app/src/bridge.rs \
  '    let (views, d) = load_views(config);' \
  '    let (views, d) = ViewSpec::from_doc(config.doc("views").expect("checked above"));' \
  geode-app \
  data_setup_hands_out_views_with_the_users_presentation_already_merged

# A desk renaming a column, or retiring a view, must never turn a
# trader's personal file into an error. Promoting the mismatch to an
# Error is the plausible mistake, because it reads like rigour.
#
# Be precise about what this mutation actually costs, because nothing
# branches on the severity TODAY: `load_views`' diagnostics never reach
# `Config::diagnostics`, so `reload::decide` does not see them; at
# startup `bridge::start` prints them as `[data] ...` lines whatever
# their severity, and on the reload path they are discarded outright.
# The test therefore asserts the classification directly rather than
# through a downstream consequence — which is exactly why this needs an
# entry rather than being left to a consequence test. A wrong severity
# here is invisible until the first thing that routes by severity
# arrives (the Views dialog's per-field diagnostics, spec 1.2's
# repurposed `Diagnostic.path`), and by then it has been the shipped
# behaviour for months.
#
# The `warn` closure this anchors on is shared by the view-level
# "no view of that name" case below it, so one line carries the
# classification for both stale-name shapes; the second entry covers
# that case's own skip-vs-drop behaviour, which this one cannot see.
run_mutation "views: a presentation naming a column the view lacks warns, it does not error" \
  crates/geode-core/src/view.rs \
  '            let warn = |m: String| Diagnostic {
                severity: Severity::Warning,' \
  '            let warn = |m: String| Diagnostic {
                severity: Severity::Error,' \
  geode-core \
  a_column_the_view_lacks_is_a_warning_not_an_error

# The other half of the same rule, and a different failure: not the
# severity but whether the mismatch is reported at all. A presentation
# table keyed to a view name nothing answers to is what a desk leaves
# behind every time it renames or retires a view, and `continue`-ing
# without the warning is the tidy-looking version. It would be silent:
# the personalisation simply stops applying and nothing anywhere says
# why, which is the one outcome a trader cannot debug.
run_mutation "views: a presentation naming a view the config lacks is skipped LOUDLY" \
  crates/geode-core/src/view.rs \
  '                diags.push(warn("no view of that name — ignored".into()));' \
  '' \
  geode-core \
  a_view_the_config_lacks_is_a_warning_not_an_error

# `view_presentation` changes the ViewSpecs a tile runs on exactly as a
# `views` edit does, and it is the doc the Views dialog writes on the
# commonest edit there is. Left out of `views_changed`, the write lands
# on disk and nothing on screen moves until the next restart.
run_mutation "reload: a view_presentation change triggers the same reload a views change does" \
  crates/geode-shell/src/shell/hot_reload.rs \
  'changed("views") || changed("view_presentation") || changed("dimensions");' \
  'changed("views") || changed("dimensions");' \
  geode-shell \
  a_view_presentation_only_change_emits_config_reloaded

# ---- Phase 4c: the object dialog's browse stage
#
# The marker that decides whether Task 5 offers a DESTRUCTIVE action.
# "Revert to desk" deletes the object from the user layer; on a view only
# the user layer defines, that deletes the view outright instead of
# restoring anything. Computing `overridden` from the winning layer alone
# — "the user layer defines it" — is the tidy-looking version and is
# green against every fixture where a user override shadows something,
# because there it gives the right answer for the wrong reason. Only a
# user-ONLY object separates the two.
run_mutation "objectdialog: overridden is computed from the winning layer alone" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '            row.overridden = mine && layers.iter().any(|l| *l < Layer::User);' \
  '            row.overridden = mine;' \
  geode-shell \
  a_user_only_object_is_not_marked_overridden

# The other half of the same line, and the fix for a Critical the browse
# fixtures could not reach: `mine` has to span the PRESENTATION doc too.
# §4.1's split guarantees that hiding a column writes
# `view_presentation.toml` and leaves the domain's own user layer empty,
# so a marker read off `doc` alone is green against every fixture where a
# user override shadows something and wrong on the commonest edit there
# is — `r` answered "no user override to revert" about a file sitting on
# disk. Only a fixture with a user-layer presentation entry and NO
# user-layer views entry separates the two.
run_mutation "objectdialog: overridden ignores a presentation-only override" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '            let mine = layers.contains(&Layer::User) || personalised.contains(row.name.as_str());' \
  '            let mine = layers.contains(&Layer::User);' \
  geode-shell \
  a_presentation_only_override_is_marked_overridden

# And the notice `d` answers with once that marker is right. Dropping the
# tail leaves every delete/revert test green — the gate is unchanged and
# nothing is written either way — while the refusal goes back to telling a
# trader who has a personalisation on disk that there is nothing of theirs,
# and naming no verb that would undo it.
run_mutation "objectdialog: d denies an override it can see" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '                " — but r reverts your changes to it"' \
  '                ""' \
  geode-shell \
  revert_undoes_a_presentation_only_override

# The opening mode is one line, and it silently restores the pre-modal
# model: every filter test still passes with the dialog opening
# filter-first (`/` is harmless when the field is already focused), and
# the rows still paint. What breaks is invisible from those tests — the
# letters become text again, so `j`/`k` type instead of moving and Task
# 5's verbs would have nowhere to live. Only a test asserting a bare
# letter did NOT reach the filter can see it.
run_mutation "objectdialog: the dialog opens in filter mode" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '            mode: DialogMode::Normal,' \
  '            mode: DialogMode::Filter,' \
  geode-shell \
  config_views_opens_in_normal_mode_and_lists_the_views

# The query has to reach the ranking, not just be stored. With the rank
# run against an empty query the state assertions all stay green — the
# mode changes, the query is mirrored, the escape ladder still walks —
# and the list simply never narrows, which only a test asserting a
# non-matching row stopped PAINTING can see.
run_mutation "objectdialog: the browse list ignores the query it displays" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '    crate::listfilter::rank(&texts, &state.query)' \
  '    crate::listfilter::rank(&texts, "")' \
  geode-shell \
  slash_filters_and_escape_walks_the_ladder

# `close_modal` is the one door every modal close goes through, and the
# shared `dialog_input` subscription routes a query edit to "whichever
# dialog state is Some". A close that leaves `object_dialog` behind is
# invisible until the NEXT dialog opens, at which point that stale state
# swallows its queries — the exact bug that field's own doc comment
# records for `settings`.
run_mutation "objectdialog: closing the modal leaves the dialog's state behind" \
  crates/geode-shell/src/shell/mod.rs \
  '        self.object_dialog = None;' \
  '' \
  geode-shell \
  slash_filters_and_escape_walks_the_ladder

# ---- Phase 4c: the object dialog's edit stage
#
# THE destination split (spec §4.1), and the most expensive thing on this
# surface to get wrong. Sending the column list to `Doc` is the tidy-looking
# version — one destination, one file — and every unit assertion about
# order, hiding and width still passes, because the draft is unchanged and
# only the FILE it lands in moves. What it silently does is fork the desk's
# view into the user's `views.toml` the first time a trader hides a column,
# and a forked view is frozen: the desk adds a column next week and this
# trader never sees it. Only a test asserting that `views.toml` was NOT
# created can see it.
run_mutation "objectdialog: a presentation field is written to the view's own doc" \
  crates/geode-shell/src/shell/objectdialog/views.rs \
  '            dest: Destination::Presentation,' \
  '            dest: Destination::Doc,' \
  geode-shell \
  hiding_a_column_writes_presentation_and_does_not_fork_the_view

# Spec §7.2: validate the object being edited, not the merged result.
# Validating the merged doc instead is green against any fixture with one
# view in it — the diagnostics are identical — and only diverges when some
# OTHER view in the config is broken, at which point the dialog reports a
# stranger's problem against the object on screen and the user has nothing
# to fix. The covering test needs two views, one of them broken, which is
# exactly the fixture a single-view test would never build.
run_mutation "objectdialog: validation runs against the merged doc, not the draft" \
  crates/geode-shell/src/shell/objectdialog/views.rs \
  '    let table = rendered_doc_table(draft);' \
  '    let table = config.doc(DOC).map(|d| d.value.clone()).unwrap_or_default();' \
  geode-shell \
  validation_sees_the_draft_and_not_the_rest_of_the_config

# ---- Phase 4c: instant config edits (spec §3.2/§7.1)
#
# Two entries were REMOVED here rather than re-anchored, because the
# behaviour they defended is gone rather than moved:
#
#   * "a field edit writes immediately instead of staging" — staging is
#     what this design deletes. Its covering test
#     (`a_field_edit_stages_and_writes_nothing_until_save`) asserted the
#     opposite of the requirement and is replaced by
#     `a_field_edit_applies_instantly_and_the_file_follows`, defended by
#     the entry below.
#   * "escape on a dirty draft skips the confirm" — `leave_or_confirm`
#     and `Confirm::Discard` no longer exist. With every edit applied on
#     its own keystroke there is no unsaved work for `escape` to
#     discard, so there is no branch left to break.
#
# THE requirement ("changing a config field is INSTANT"), and the one
# mutation that puts the old design back: apply the change by writing the
# file and letting the loader read it home. Every assertion about the
# resulting VALUE stays green — a round trip through disk produces the
# same merged config in the end — and what silently returns is the half
# second between the keystroke and the screen, plus a config rebuilt from
# whatever else happens to be in the user's directory. Only a fixture
# whose disk DISAGREES with memory can tell the two apart, which is why
# the covering test plants a decoy `views.toml` the running config has
# never read.
run_mutation "objectdialog: an edit round-trips through disk instead of merging in memory" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '    let mut docs = shell.services.config.all_docs();
    for ((doc, object), value) in edits {
        docs = docs_with_object(docs, user_dir, doc, object, value.clone());
    }
    let config = Config::from_docs(docs);' \
  '    let _ = edits;
    let config = Config::load(&geode_core::config::ConfigSources {
        builtin: shell.services.builtin.clone(),
        desk: None,
        user: Some(user_dir.to_path_buf()),
    });' \
  geode-shell \
  an_edit_merges_in_memory_without_reading_disk

# Applying stays singular (spec §7.1). Assigning `services.config`
# directly is the tempting shortcut — the dialog is the thing that
# changed, and it re-derives from `services.config` on every render, so
# BOTH stages repaint correctly and every value assertion here stays
# green. What is skipped is everything else `apply_reload` does: the
# `ShellEvent::ConfigReloaded` the app bridge turns into the `ViewSpec`s
# the data thread runs on. The trader hides a column, the dialog agrees
# it is hidden, and every blotter tile keeps querying the old view.
run_mutation "objectdialog: an edit assigns the config instead of going through apply_reload" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '    shell.apply_reload(config, cx);' \
  '    shell.services.config = config;' \
  geode-shell \
  the_config_fan_out_is_debounced_and_goes_through_the_one_applier

# The fan-out rides the write's timer (spec §7.1, review ruling). Applying
# on the keystroke is what the first build did and it is invisible to
# every value assertion — the config ends up identical, just sooner and N
# times instead of once. What it costs is a `ConfigReloaded` per
# keystroke: the bridge re-derives views and EVERY blotter tile requeries,
# a §7.1 <50 ms operation, at the OS key-repeat rate under a held key.
# Only a test counting the events can see it.
run_mutation "objectdialog: the config fan-out fires per keystroke instead of riding the debounce" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '    schedule_flush(shell, user_dir, edits, revert, delay, cx);' \
  '    apply_in_memory(shell, &user_dir, &edits, cx);
    schedule_flush(shell, user_dir, edits, revert, delay, cx);' \
  geode-shell \
  the_config_fan_out_is_debounced_and_goes_through_the_one_applier

# CRITICAL, and a data-loss path: the success arm has to respect the
# sequence it was launched with. Clearing the pending batch
# unconditionally erases any edit that arrived while the write was in
# flight — that edit reaches neither memory nor disk, and the watcher,
# woken by the write that DID land, then reverts memory to the older
# on-disk state. Every ordinary test stays green, because in the test
# executor a `background_executor().spawn` is polled inline and no
# keystroke can land inside the window at all; the covering test drives
# `finish_flush` with a stale sequence on real state for exactly that
# reason.
run_mutation "objectdialog: a stale write completion clears a newer edit's batch" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '            if shell
                .pending_config_write
                .as_ref()
                .is_some_and(|pending| pending.seq == seq)
            {
                shell.pending_config_write = None;
            }' \
  '            let _ = seq;
            shell.pending_config_write = None;' \
  geode-shell \
  a_stale_write_completion_does_not_erase_a_newer_edit

# `reload::decide` rejects any config holding an error diagnostic, so
# carrying the previous config's forward makes ONE unparseable file in
# the user directory turn every dialog edit into a silent in-memory
# no-op — while the write still fires, so memory and disk diverge with
# nothing on screen saying why. Every test with a healthy config stays
# green; only a fixture that starts with a broken file can see it.
run_mutation "objectdialog: an edit carries the previous config's diagnostics forward" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '    let config = Config::from_docs(docs);
    shell.apply_reload(config, cx);' \
  '    let mut config = Config::from_docs(docs);
    config.diagnostics = shell.services.config.diagnostics.clone();
    shell.apply_reload(config, cx);' \
  geode-shell \
  an_edit_applies_even_when_another_config_file_is_broken

# A failed write with no dialog open. `PendingConfigWrite` lives on
# `ShellView` precisely so a write outlives the dialog that started it —
# a trader can close the dialog inside the 250 ms window — so the
# dialog-only notice is absent on exactly the path this exists to cover,
# and the revert would happen in silence. The dialog-open test stays
# green either way.
run_mutation "objectdialog: a failed write reports only through the dialog notice" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '    shell.config_write_error = Some(format!("config not saved — reverted: {message}"));' \
  '' \
  geode-shell \
  a_write_that_fails_after_the_dialog_closed_still_reports_itself

# The empty-table ruling. `views::presentation_table` renders an EMPTY
# table whenever the trader's presentation matches the view's own doc —
# which is one keystroke away, every time the last hidden column is
# unhidden — and writing it produces a bare `[tree]` in
# `view_presentation.toml`: a table that says nothing, which
# `ViewPresentationSpec::apply` then reports as a stale entry naming a
# view. Green against every other assertion, because an empty table
# merges to the same result as no table at all. Only a test asserting the
# object is ABSENT can see it.
run_mutation "objectdialog: an empty object table is written instead of removed" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '    if item_is_empty(&item) {
        return match dest {' \
  '    if false {
        return match dest {' \
  geode-shell \
  unhiding_the_last_column_removes_the_object_rather_than_writing_an_empty_table

# Hazard 1: a failed background write leaves memory ahead of disk. Logging
# and moving on is what every other persist path in this crate does, and
# it was right there — those paths write what the user already asked for
# and nothing on screen depends on the result. Here memory has ALREADY
# applied the change, so a swallowed failure leaves the trader looking at
# a value that exists nowhere but this process, with no notice and no way
# to find out. Every green-path test stays green: the failure only happens
# when the file cannot be written at all.
run_mutation "objectdialog: a failed write is logged instead of reverting memory" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '        Err(message) => revert_failed_write(shell, message, cx),' \
  '        Err(message) => eprintln!("[config] warning: {message}"),' \
  geode-shell \
  a_failed_write_reverts_the_in_memory_change_and_says_so

# The write debounce. Without it every value assertion still passes — the
# file ends up holding the same final state — and what returns is a write
# per keystroke: a held `shift+j` reordering a column at the OS key-repeat
# rate rewrites `view_presentation.toml` ten times a second, each one
# firing the mtime watcher. Only a test asserting that NOTHING reached
# disk while the keys were still coming can see it.
run_mutation "objectdialog: the config write fires per keystroke instead of coalescing" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '        cx.background_executor().timer(delay).await;' \
  '' \
  geode-shell \
  edits_inside_the_debounce_window_coalesce_into_one_write

# Hazard 2's proof, and the one line it rests on. A user-layer document
# memory creates for a file that does not exist yet has to carry the same
# `config_version` stamp `config_write::edit` puts at the top of a file it
# creates — otherwise the document memory holds and the document the
# watcher reads back a moment later differ by one key, `apply_reload`'s
# `changed(..)` answers true, and the self-write reload stops being the
# no-op this design chose to prove instead of suppress: a `ConfigReloaded`
# emit and every tile requerying, a beat after a keystroke that had
# already finished. Invisible to every test that only reads values.
run_mutation "objectdialog: a user doc created in memory carries no config_version" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '    table.insert(
        "config_version".to_string(),
        toml::Value::Integer(CONFIG_VERSION),
    );' \
  '' \
  geode-shell \
  the_watchers_reload_of_our_own_write_changes_nothing

# The one edit that still asks first. Forking is instant and irreversible
# in the direction that matters — a user-layer copy of a desk view stops
# receiving the desk's changes — and applying it without the confirm is
# green against every test that only checks the value landed: it DID
# land, in the user's own `views.toml`, weeks before anyone notices the
# desk's new column never arrived.
run_mutation "objectdialog: a definitional change forks without asking" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    if super::apply::would_fork(shell, domain) {' \
  '    if false {' \
  geode-shell \
  a_definitional_change_to_a_desk_view_confirms_before_forking

# Spec §7.1's no-carry-forward rule means `reload::decide` rejects any
# config holding an error diagnostic, so an error-severity edit that
# still joined the pending batch would be a silent in-memory no-op a
# flush later — while the file write fires anyway, leaving memory and
# disk disagreeing. Matching against a severity nothing in this draft's
# `diagnostics` ever holds is the same as deleting the gate: the `find`
# never matches, so the batch check below it never sees a reason to
# refuse. Every other objectdialog test stays green (none of them ever
# put an error diagnostic on a draft); only a test that does can see it.
# `blocking_diagnostic` (apply.rs) is the one check both call sites into
# `commit_edit` share (render.rs's `commit_or_confirm` peeks it early for
# UX; `commit_edit` itself checks it again, unconditionally). Disabling
# it here disables the gate everywhere at once — this is CI's regression
# test for the whole rule, not just one caller of it.
run_mutation "objectdialog: an error diagnostic no longer blocks the batch" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '.find(|d| d.severity == Severity::Error)?;' \
  '.find(|_d| false)?;' \
  geode-shell \
  an_edit_the_reader_rejects_does_not_join_the_batch

# The other failure mode of the same rule: gate on "has a diagnostic" —
# no severity check at all — rather than "has an error", and a
# `Warning` blocks too. A desk renaming a column produces a `Warning`
# by design (`views::validate`'s dataset check); this is precisely the
# mutation that would make that trader's personal `view_presentation.
# toml` unsaveable through the dialog built to manage it, and every
# blocking test above stays green because it only ever puts an
# `Error` on the draft — none of them can see a severity check
# widening.
run_mutation "objectdialog: a warning blocks the batch as if it were an error" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '.find(|d| d.severity == Severity::Error)?;' \
  '.find(|_d| true)?;' \
  geode-shell \
  an_edit_with_only_warnings_still_joins_the_batch

# Task 3's own regression: `d`/`r` used to write the file straight off
# the render thread with no in-memory merge at all (`spawn_removals`),
# so a delete or revert sat invisible in the browse list until the 500 ms
# watcher noticed the write and reloaded — the one mutation left in this
# dialog after every field edit went instant. This mutation puts that
# exact shape back: `commit_removal` writes through `run_writes` on the
# background executor and returns, skipping `queue_batch` — so `services.
# config` never changes and the browse row survives its own deletion.
# Every notice- and file-only assertion stays green (the file still ends
# up right, the confirm still fires); only a test reading the row back
# out of the browse list, with nothing but `run_until_parked` driving it,
# can see the row that never left.
#
# Re-anchored on `commit_removal`'s own "nothing was removed" wording
# (Task 3, this codebase): `commit_create`'s `queue_batch(..
# Duration::ZERO..); None }` tail reads identically, and the bare 3-line
# form this entry used to anchor on stopped being unique the moment that
# sibling function existed.
run_mutation "objectdialog: a removal bypasses the batch again" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '    let Some(user_dir) = shell.user_dir.clone() else {
        return Some("no writable user config directory — nothing was removed".to_string());
    };
    queue_batch(shell, edits, user_dir, Duration::ZERO, cx);
    None
}' \
  '    let Some(user_dir) = shell.user_dir.clone() else {
        return Some("no writable user config directory — nothing was removed".to_string());
    };
    cx.background_executor()
        .spawn(async move {
            let _ = run_writes(&user_dir, edits);
        })
        .detach();
    None
}' \
  geode-shell \
  deleting_a_user_layer_object_leaves_the_browse_list_before_the_watcher_could_fire

# `Config::all_docs` is the other half of the loader split: it is what
# hands `from_docs` the documents to merge, and dropping the layers below
# the user's is the plausible "we only changed the user layer" shortcut.
# The merged result is still a valid config and still contains the edit,
# so a value assertion passes; what vanishes is every desk and builtin
# document — the exact shape of the shipped reload bug this codebase
# already paid for once, one function further in.
run_mutation "config: all_docs hands the merge the user layer alone" \
  crates/geode-core/src/config/mod.rs \
  '        self.layered.values().flatten().cloned().collect()' \
  '        self.layered
            .values()
            .flatten()
            .filter(|d| d.layer == Layer::User)
            .cloned()
            .collect()' \
  geode-core \
  from_docs_merges_exactly_as_load_does

# The name rule is one function so the dialog and `:scope save` cannot
# disagree. Dropping the `config_version` clause lets a trader create an
# object named after the doc's own schema stamp.
run_mutation "config: check_object_name refuses config_version" \
  crates/geode-core/src/config/mod.rs \
  '        || name == "config_version"' \
  '        || false' \
  geode-core \
  object_names_follow_the_frames_rule

# The edit stage is always normal mode. `enter` opens an object from
# FILTER mode too, and a stage left in `Filter` sends the next `escape`
# down the ladder's `LeaveFilter` rung — which `handle_edit_key` does not
# claim, so the shell's modal branch closes the whole dialog and takes the
# unsaved draft with it, without ever asking. Every escape test that opens
# its object from normal mode stays green; only one that opens it out of a
# filtered list can see it.
#
# Re-anchored on `enter_edit`'s own preceding lines (Task 3): Task 3's
# `cancel_naming` sets the exact same `self.mode = DialogMode::Normal;` at
# the same indent, and the bare-line anchor stopped being unique the
# moment that sibling method existed.
#
# Re-anchored again for §18.8 (2026-09-12): the mode is now a two-armed
# `if` keyed on the domain, and the mutation keeps Groupings' `Filter`
# arm — a slot still opens in its chain field, so every Groupings test
# stays green — while letting every other domain inherit whatever mode
# browse was in, which is exactly the old defect.
run_mutation "objectdialog: the edit stage inherits the browse filter's mode" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        self.mode = if opens_in_chain_field {
            DialogMode::Filter
        } else {
            DialogMode::Normal
        };' \
  '        if opens_in_chain_field {
            self.mode = DialogMode::Filter;
        }' \
  geode-shell \
  an_object_opened_from_filter_mode_still_escapes_back_a_stage

# ---- Phase 4c review wave: hiding a column, and not freezing the desk
#
# THE headline capability of the presentation design (spec §1.3): "hiding
# one … and the blotter reflects it without a restart". Nothing consumed
# `ColumnPresentation.hidden` when Part 1 shipped, and the end-to-end test
# could not see it because it asserted the WRITE, not the effect — the
# dialog wrote `view_presentation.toml` correctly, the watcher reloaded,
# the merge applied, and the column was still on screen. Every link in
# that chain has its own green test. Only a test asserting the column is
# absent from what the blotter PLANS to paint can see this one.
run_mutation "plan: a hidden column is planned anyway" \
  crates/geode-blotter/src/core/plan.rs \
  '            if presentation.hidden.unwrap_or(false) {' \
  '            if false {' \
  geode-blotter \
  a_hidden_column_is_not_planned

# The freeze `Destination` exists to prevent, one field-granularity down.
# `order` arrives from the EFFECTIVE view, so writing it unconditionally
# pins the desk's own column order into the trader's personal file the
# first time they hide anything — and every assertion about what the file
# contains stays green, because the order written is the right order
# TODAY. Only a test asserting the key is ABSENT when nothing was
# reordered can see it.
run_mutation "views: a presentation save pins the desk's column order" \
  crates/geode-shell/src/shell/objectdialog/views.rs \
  '    if names != doc_order {' \
  '    if true {' \
  geode-shell \
  a_presentation_save_writes_only_what_the_trader_changed

# The same freeze for `width`, and the half that is latent today: nothing
# in Geode can set a width yet, so a suite that only ever sees widths the
# desk declared cannot tell "kept the trader's" from "copied the desk's".
# The fixture has to declare a width in `views.toml` and change something
# else entirely.
run_mutation "views: a presentation save copies the desk's widths into the user's file" \
  crates/geode-shell/src/shell/objectdialog/views.rs \
  '        if doc_widths.get(item.name.as_str()) == Some(&width) {' \
  '        if false {' \
  geode-shell \
  a_presentation_save_writes_only_what_the_trader_changed

# ---- Phase 4c: the reload keeps the app's own builtin layer ----------

# The shipped bug, and the reason this entry exists at all: the reload
# rebuilt the builtin layer as "the builtin keymap, surely" instead of
# reusing the docs the process started with. Under `--demo` that layer is
# a whole generated desk, so the first config write of a session — a theme
# toggle, a font-size change, a dialog save — silently deleted the views,
# datasets and sources, and the views dialog then reported "no views are
# configured". Every test in the suite was green throughout: none of them
# had a builtin layer wider than the keymap, so none could tell reuse from
# reconstruction. The mutation below is exactly the old code.
run_mutation "reload: the builtin layer is reused, not rebuilt from the keymap alone" \
  crates/geode-shell/src/reload.rs \
  '    Config::load(&ConfigSources {
        builtin,
        desk,
        user,
    })' \
  '    Config::load(&ConfigSources {
        builtin: builtin
            .into_iter()
            .filter(|doc| doc.name == "keymap")
            .collect(),
        desk,
        user,
    })' \
  geode-shell \
  a_reload_keeps_every_builtin_doc_not_just_the_keymap

# ---- Phase 4b Task 1: the deferred 4a-review minors (M2, M5, M7, M8,
# M10, M11, M12, M13) ---------------------------------------------------

run_mutation "M2: a text session that ends where it began pops its own undo entry" \
  crates/geode-shell/src/frame.rs \
  '    pub fn end_scope_session(&mut self) {
        if let Some(session) = self.scope_session.take()
            && session.pushed
            && self.scope_undo.last() == Some(&session.base)
            && self.scope == session.base
        {
            self.scope_undo.pop();
            self.scope_redo = session.redo_snapshot;
        }
    }' \
  '    pub fn end_scope_session(&mut self) {
        self.scope_session = None;
    }' \
  geode-shell a_text_session_that_ends_where_it_began_leaves_no_undo_entry

run_mutation "M5: the picker's tag is session-wide, not per-open" \
  crates/geode-shell/src/shell/picker.rs \
  '    view.next_picker_tag += 1;
    let tag = view.next_picker_tag;' \
  '    let tag = view.next_picker_tag;' \
  geode-shell a_second_open_on_the_same_column_carries_a_larger_tag_than_the_first

run_mutation "M7: the flip barrier is swept on the reload-poll tick" \
  crates/geode-shell/src/shell/mod.rs \
  '                let Ok(frame) = this.update(cx, |view, _cx| view.frame.clone()) else {
                    return; // window/entity gone; stop polling
                };
                frame.update(cx, |f, cx| {
                    if f.sweep(Instant::now()) {
                        cx.notify();
                    }
                });' \
  '' \
  geode-shell the_reload_poll_tick_sweeps_an_open_barrier_past_its_deadline

run_mutation "M8: a placeholder occupant is excluded from the barrier's key set" \
  crates/geode-shell/src/shell/occupants.rs \
  '        let has_real_occupant = |id: &TileId| {
            self.occupants
                .get(id)
                .is_some_and(|o| o.kind != PLACEHOLDER_KIND)
        };' \
  '        let has_real_occupant = |id: &TileId| self.occupants.contains_key(id);' \
  geode-shell a_placeholder_occupant_is_excluded_from_the_barriers_key_set

run_mutation "M10: Expr Display escapes an embedded quote in a string literal" \
  crates/geode-core/src/scope/expr.rs \
  "                    if c == '\\'' {" \
  "                    if false {" \
  geode-core a_quote_inside_a_string_literal_escapes_as_a_doubled_quote_and_round_trips

run_mutation "M11: save_scope bumps saved_scopes, not config" \
  crates/geode-shell/src/frame.rs \
  '        self.versions.saved_scopes += 1;' \
  '        self.versions.config += 1;' \
  geode-shell save_scope_bumps_saved_scopes_not_config

run_mutation "M12: the bar-model cache key includes today's date" \
  crates/geode-shell/src/frame.rs \
  '        if let Some((cached_versions, cached_today, cached)) = self.bar_cache.borrow().as_ref()
            && *cached_versions == versions
            && *cached_today == today
        {' \
  '        if let Some((cached_versions, _cached_today, cached)) = self.bar_cache.borrow().as_ref()
            && *cached_versions == versions
        {' \
  geode-shell the_bar_model_cache_rebuilds_when_today_changes_with_versions_unchanged

run_mutation "M13: FrameRecord omits an empty dimensions table" \
  crates/geode-shell/src/session.rs \
  '        if !dims.is_empty() {
            t.insert("dimensions".into(), toml::Value::Table(dims));
        }' \
  '        t.insert("dimensions".into(), toml::Value::Table(dims));' \
  geode-shell a_frame_record_with_no_dimension_selections_writes_no_dimensions_key

# ---- Phase 4b Task 1 review fix round 1 (MIN-12): M6 and M9 had test
# coverage but no harness entry of their own; M15 is a signature-
# preserving re-export and genuinely needs none. -----------------------

run_mutation "M6: an explicit default view wins over the alphabetical first" \
  crates/geode-core/src/view.rs \
  '                Some(v) => v.is_default = true,' \
  '                Some(_v) => {}' \
  geode-blotter a_fresh_tile_opens_on_the_explicit_default_view_not_the_alphabetical_first

run_mutation "M9: the as-of presets cache is keyed on the frame's data version" \
  crates/geode-shell/src/shell/asof_view.rs \
  '    let v = frame.versions().data;
    if let Some((cached_v, cached)) = state.presets_cache.borrow().as_ref()
        && *cached_v == v
    {
        return Rc::clone(cached);
    }' \
  '    let v = frame.versions().data;
    if let Some((_cached_v, cached)) = state.presets_cache.borrow().as_ref() {
        return Rc::clone(cached);
    }' \
  geode-shell cached_presets_rebuilds_only_when_the_frames_data_version_changes

# ---- Phase 4b Task 2: the tracing foundation (the ring, [log]) --------

run_mutation "the ring's push wraps to the next slot, not slot 0" \
  crates/geode-core/src/log/mod.rs \
  '        g.head = (head + 1) % cap;' \
  '        g.head = head;' \
  geode-core wrapping_overwrites_the_oldest_and_keeps_order

run_mutation "drain_since excludes the record at exactly since, not only older ones" \
  crates/geode-core/src/log/mod.rs \
  '                Some(r) if r.seq > since => out.push(r.clone()),' \
  '                Some(r) if r.seq >= since => out.push(r.clone()),' \
  geode-core drain_since_returns_only_newer_records_oldest_first

run_mutation "seq is assigned by the ring's push, not carried from the caller" \
  crates/geode-core/src/log/mod.rs \
  '        g.seq += 1;
        r.seq = g.seq;
        let cap = g.records.len();' \
  '        g.seq += 1;
        let cap = g.records.len();' \
  geode-core two_writers_never_lose_a_sequence_number

run_mutation "[log] rejects a key that names no known target" \
  crates/geode-core/src/log/mod.rs \
  '            if !TARGETS
                .iter()
                .any(|t| t.strip_prefix("geode::") == Some(key.as_str()))
            {
                diags.push(warn(format!(
                    "[log] {key}: not a known target ({})",
                    TARGETS
                        .iter()
                        .map(|t| &t[7..])
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
                continue;
            }
            levels.targets.retain(|(k, _)| k != key);' \
  '            levels.targets.retain(|(k, _)| k != key);' \
  geode-core an_unknown_target_key_is_a_warning

# ---- Phase 4b Task 2 fix round 1 ---------------------------------------

run_mutation "log_health_event: Health::Failed logs at warn, not error" \
  crates/geode-data/src/service.rs \
  '        Health::Failed { .. } => {
            tracing::error!(target: "geode::ingest", "{source}: {} — {detail}", worst.label());
        }' \
  '        Health::Failed { .. } => {
            tracing::warn!(target: "geode::ingest", "{source}: {} — {detail}", worst.label());
        }' \
  geode-data a_failed_health_logs_at_error_through_the_service_sink

run_mutation "log_ingest_failure: an IngestEvent::Failed logs at warn, not error" \
  crates/geode-data/src/service.rs \
  'fn log_ingest_failure(dataset: &str, batch: &str, reason: &str) {
    tracing::error!(target: "geode::ingest", "{dataset}/{batch}: {reason}");
}' \
  'fn log_ingest_failure(dataset: &str, batch: &str, reason: &str) {
    tracing::warn!(target: "geode::ingest", "{dataset}/{batch}: {reason}");
}' \
  geode-data a_load_failure_logs_dataset_batch_and_reason_at_error

run_mutation "MessageVisitor::record_str drops a non-message field instead of appending it" \
  crates/geode-core/src/log/mod.rs \
  '    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        use std::fmt::Write;
        if field.name() == "message" {
            self.0.push_str(value);
        } else {
            if !self.0.is_empty() {
                self.0.push('"'"' '"'"');
            }
            let _ = write!(self.0, "{}={value}", field.name());
        }
    }' \
  '    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        }
    }' \
  geode-core a_non_message_str_field_is_not_dropped

run_mutation "trim_log_files deletes the newest files beyond the cap, not the oldest" \
  crates/geode-app/src/crash.rs \
  '    for old in &files[..files.len() - keep] {' \
  '    for old in &files[keep..] {' \
  geode-app trim_deletes_the_oldest_files_beyond_the_cap

run_mutation "[log] present but not a table is silently accepted, not a warning" \
  crates/geode-core/src/log/mod.rs \
  '        let Some(table) = value.as_table() else {
            diags.push(warn(
                "[log]: expected a table, e.g. [log]\\ndefault = \"info\"".to_string(),
            ));
            return (levels, diags);
        };' \
  '        let Some(table) = value.as_table() else {
            return (levels, diags);
        };' \
  geode-core log_present_but_not_a_table_is_a_warning

run_mutation "to_targets: every crate follows [log] default, not just geode" \
  crates/geode-core/src/log/mod.rs \
  '        let mut t = Targets::new()
            .with_default(Level::WARN)
            .with_target("geode", self.default);' \
  '        let mut t = Targets::new().with_default(self.default);' \
  geode-core to_targets_caps_non_geode_targets_at_warn_regardless_of_default

run_mutation "drain_since's backward scan never stops early, always visiting every slot" \
  crates/geode-core/src/log/mod.rs \
  '                Some(r) if r.seq > since => out.push(r.clone()),
                // Either an empty slot (the ring hasn'"'"'t wrapped yet, and
                // we'"'"'ve walked past its oldest write) or a record at or
                // before `since` — descending order means nothing
                // further back can be newer than `since` either.
                _ => break,' \
  '                Some(r) if r.seq > since => out.push(r.clone()),
                _ => continue,' \
  geode-core drain_since_stops_scanning_once_it_reaches_records_at_or_before_since

run_mutation "oldest_seq ignores the wrap and always reads slot 0" \
  crates/geode-core/src/log/mod.rs \
  '    pub fn oldest_seq(&self) -> Option<u64> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match &g.records[g.head] {
            Some(r) => Some(r.seq),
            None => g.records[0].as_ref().map(|r| r.seq),
        }
    }' \
  '    pub fn oldest_seq(&self) -> Option<u64> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.records[0].as_ref().map(|r| r.seq)
    }' \
  geode-core oldest_seq_is_none_when_empty_then_tracks_the_surviving_floor_through_a_wrap

run_mutation "build_catalog: live is the first generation per partition, not the last" \
  crates/geode-data/src/query/catalog.rs \
  '        if let Some(newest) = p.generations.last_mut() {
            newest.live = true;
        }' \
  '        if let Some(newest) = p.generations.first_mut() {
            newest.live = true;
        }' \
  geode-data the_catalog_lists_every_partitions_generations_with_the_live_one_marked

run_mutation "build_catalog: resolved_gen is ignored under AsOf::At, always None" \
  crates/geode-data/src/query/catalog.rs \
  '            p.resolved_gen = by_partition
                .get(&(p.batch.clone(), p.book.clone()))
                .copied();' \
  '            p.resolved_gen = None;' \
  geode-data under_an_as_of_the_resolved_generation_is_named_per_partition

# MIN-1/MIN-2 (review round 1): retargeted at `archive_rows` rather than
# `live_rows` — the `live_rows` direction of this mutation was already
# caught twice over (this test's own oracle *and*
# `the_catalog_lists_every_partitions_generations_with_the_live_one_marked`'s
# `ds.live_rows == 5`), so neither test was proven isolated. Mutating
# `archive_rows` instead has exactly one defence: the `archive_rows`
# assertion this fix round added here.
run_mutation "build_catalog: archive_rows sums the live tables too" \
  crates/geode-data/src/query/catalog.rs \
  '        archive_rows += sizes
            .get(&table_name(&ds.name, grain, TableKind::Archive))
            .copied()
            .unwrap_or(0);
    }' \
  '        archive_rows += sizes
            .get(&table_name(&ds.name, grain, TableKind::Live))
            .copied()
            .unwrap_or(0);
        archive_rows += sizes
            .get(&table_name(&ds.name, grain, TableKind::Archive))
            .copied()
            .unwrap_or(0);
    }' \
  geode-data the_row_counts_agree_with_duckdb_by_execution

# Review round 1 MAJ-1: the fixture's `file_id`s (11, 12, 13) are
# deliberately not the same as the `gen_id`s (1, 2, 3) they name, so a
# join keyed on the wrong column no longer lines up by accident.
run_mutation "build_catalog: loaded_at/file_rows join on file_id, not gen_id" \
  crates/geode-data/src/query/catalog.rs \
  '    let sql = "select fg.gen_id, fg.loaded_at, fg.row_count \' \
  '    let sql = "select fg.file_id, fg.loaded_at, fg.row_count \' \
  geode-data the_catalog_lists_every_partitions_generations_with_the_live_one_marked

# Review round 1 MAJ-2: `file_generations_for` must stay bounded to the
# generations `generations` still names, not every row `file_generations`
# has ever accumulated (that table is never pruned). Removing the
# `exists` bound reproduces the unbounded read the fixture's orphan row
# (gen_id 99, no matching `generations` row) exists to catch.
run_mutation "build_catalog: file_generations_for reads every row ever recorded, not just the surviving ones" \
  crates/geode-data/src/query/catalog.rs \
  '    let sql = "select fg.gen_id, fg.loaded_at, fg.row_count \
               from file_generations fg \
               where fg.dataset = ? \
                 and exists (select 1 from generations g \
                             where g.dataset = fg.dataset and g.gen_id = fg.gen_id)";' \
  '    let sql = "select fg.gen_id, fg.loaded_at, fg.row_count \
               from file_generations fg \
               where fg.dataset = ?";' \
  geode-data file_generations_for_excludes_a_row_whose_generation_no_longer_exists

# Review round 1 MAJ-3, isolating the `batch` half of the `(batch, book)`
# partition key: `fixture_one_batch_many_books` gives EOD/BK000,
# EOD/BK001 and EOD/NULL the same batch, so comparing `batch` alone
# collapses all three into one partition.
run_mutation "build_catalog: partitions group by batch alone" \
  crates/geode-data/src/query/catalog.rs \
  '    for (batch, book, gen_id, source_time) in rows {
        let same_partition = partitions
            .last()
            .is_some_and(|p| p.batch == batch && p.book == book);' \
  '    for (batch, book, gen_id, source_time) in rows {
        let same_partition = partitions
            .last()
            .is_some_and(|p| p.batch == batch);' \
  geode-data partitions_group_by_batch_and_book_together_not_either_alone

# Review round 1 MAJ-3, isolating the `book` half: EOD/NULL and
# FOLLOWUP/NULL sort adjacently (NULL last within a batch, "EOD" <
# "FOLLOWUP") and share book `None`, so comparing `book` alone collapses
# those two into one partition across the batch boundary.
run_mutation "build_catalog: partitions group by book alone" \
  crates/geode-data/src/query/catalog.rs \
  '    for (batch, book, gen_id, source_time) in rows {
        let same_partition = partitions
            .last()
            .is_some_and(|p| p.batch == batch && p.book == book);' \
  '    for (batch, book, gen_id, source_time) in rows {
        let same_partition = partitions
            .last()
            .is_some_and(|p| p.book == book);' \
  geode-data partitions_group_by_batch_and_book_together_not_either_alone

# Review round 1 MAJ-3: the bookless partition (rows with `book is
# null`) is a real partition, spec §4.4/§4.4-adjacent code (as_of.rs,
# retention.rs, publish.rs) all carry the same warning about it.
run_mutation "build_catalog: the bookless partition is dropped" \
  crates/geode-data/src/query/catalog.rs \
  '    for (batch, book, gen_id, source_time) in rows {
        let same_partition = partitions
            .last()
            .is_some_and(|p| p.batch == batch && p.book == book);' \
  '    for (batch, book, gen_id, source_time) in rows {
        if book.is_none() {
            continue;
        }
        let same_partition = partitions
            .last()
            .is_some_and(|p| p.batch == batch && p.book == book);' \
  geode-data the_bookless_partition_is_kept_as_its_own_partition_with_its_generation_live

# Review round 1 MAJ-4(a): renamed to what this actually defends — the
# `SchedulerEvent::Polled` emit site's `next_in`, not the mapped
# `DataEvent::Polled.next` (which the entry below now covers on its
# own, restoring the brief's own Step 8 entry rather than only
# substituting for it).
run_mutation "scheduler: Polled carries a zero next_in" \
  crates/geode-data/src/ingest/scheduler.rs \
  '                        next_in: spec.poll_interval,' \
  '                        next_in: Duration::ZERO,' \
  geode-data an_unchanged_directory_submits_nothing_on_later_polls

# Review round 1 MAJ-4(b): the brief's Step 8 entry ("Polled.next = at")
# restored directly against the extracted `polled_event` helper.
run_mutation "service: Polled.next = at, next_in is ignored" \
  crates/geode-data/src/service.rs \
  '        next: at.checked_add(next_in).unwrap_or(at),' \
  '        next: at,' \
  geode-data polled_event_next_is_at_plus_next_in

# --- Task 4: the Diagnostics entity, its feed, and the status bar -----

# Phase 4b Task 4 fix round 2 (NEW-3): re-anchored — MAJ-2 (fix round
# 1) replaced the `!is_new && ...` guard this used to anchor on with
# `if let Some(current) = &state.health && ...`. Distinct from the
# MAJ-2 entry below (which targets the first-real-note edge case via a
# default-to-Ok comparison): this one disables the whole guard, so a
# *repeat* of the same health bumps — the original behaviour this entry
# has always covered.
run_mutation "diagnostics: note_health bumps unconditionally, not just on a real transition" \
  crates/geode-shell/src/diagnostics.rs \
  '        if let Some(current) = &state.health
            && *current == worst
            && state.detail == detail
        {
            return;
        }' \
  '        if let Some(current) = &state.health
            && *current == worst
            && state.detail == detail
            && false
        {
            return;
        }' \
  geode-shell a_repeated_identical_health_does_not_bump_the_version

run_mutation "diagnostics: SOURCE_HISTORY_CAP loosened from 16" \
  crates/geode-shell/src/diagnostics.rs \
  'pub const SOURCE_HISTORY_CAP: usize = 16;' \
  'pub const SOURCE_HISTORY_CAP: usize = 1600;' \
  geode-shell history_is_capped_at_sixteen_transitions

run_mutation "diagnostics: summary's LABELS silently drops degraded" \
  crates/geode-shell/src/diagnostics.rs \
  'const LABELS: [&str; 5] = ["ok", "pending", "pending_too_long", "degraded", "failed"];' \
  'const LABELS: [&str; 4] = ["ok", "pending", "pending_too_long", "failed"];' \
  geode-shell the_summary_counts_sources_by_health_and_config_errors

run_mutation "diagnostics: note_published requests a catalog regardless of watchers" \
  crates/geode-shell/src/diagnostics.rs \
  '        if self.watchers > 0 {
            self.pending_catalog_request = true;
        }' \
  '        if true {
            self.pending_catalog_request = true;
        }' \
  geode-shell a_publish_requests_a_catalog_only_while_watched

run_mutation "diagnostics: refresh_frame_hist copies the histogram while unwatched" \
  crates/geode-shell/src/diagnostics.rs \
  '        if self.watchers == 0 {
            return false;
        }' \
  '        if false {
            return false;
        }' \
  geode-shell the_frame_histogram_is_copied_only_while_watched

run_mutation "bridge: a stale Catalog outcome's tag check is disabled" \
  crates/geode-app/src/bridge.rs \
  '                    DataEvent::Catalog(outcome) => {
                        if outcome.tag != catalog_tag.get() {
                            return;
                        }' \
  '                    DataEvent::Catalog(outcome) => {
                        if false {
                            return;
                        }' \
  geode-app a_stale_catalog_outcome_is_dropped_and_the_latest_is_applied

run_mutation "hot_reload: an [log] change on reload is never applied" \
  crates/geode-shell/src/shell/hot_reload.rs \
  '            if changed("app") {' \
  '            if false && changed("app") {' \
  geode-shell a_log_table_change_on_reload_applies_it_through_level_control_once

# --- Task 4 fix round 1: CRIT-1, MAJ-1..6, MIN-2..5,8..10 -------------

run_mutation "diagnostics: CRIT-1 — an unreported source counts as pending in the summary" \
  crates/geode-shell/src/diagnostics.rs \
  '            let Some(health) = &s.health else {
                continue; // no report yet — not counted (CRIT-1)
            };' \
  '            let health = s.health.clone().unwrap_or(Health::Pending);' \
  geode-shell a_described_but_unreported_source_is_not_counted_in_the_summary

run_mutation "diagnostics: MAJ-2 — note_health's first-real-note guard defaults to Ok" \
  crates/geode-shell/src/diagnostics.rs \
  '        if let Some(current) = &state.health
            && *current == worst
            && state.detail == detail
        {
            return;
        }' \
  '        if state.health.clone().unwrap_or(Health::Ok) == worst && state.detail == detail {
            return;
        }' \
  geode-shell the_first_real_health_note_transitions_even_after_describe_source_and_note_polled

run_mutation "diagnostics: MAJ-3 — refresh_frame_hist copies an unchanged histogram while watched" \
  crates/geode-shell/src/diagnostics.rs \
  '        if self.frame_hist.count() == hist.count()
            && self.frame_hist.max_micros() == hist.max_micros()
        {
            return false;' \
  '        if false {
            return false;' \
  geode-shell refresh_frame_hist_is_a_no_op_when_the_histogram_is_unchanged

run_mutation "diagnostics: MAJ-4 — restart_required re-embedded in the summary" \
  crates/geode-shell/src/diagnostics.rs \
  '        if self.dropped_events > 0 {
            parts.push(format!("{} dropped", self.dropped_events));
        }

        parts.join(" · ")
    }' \
  '        if self.dropped_events > 0 {
            parts.push(format!("{} dropped", self.dropped_events));
        }

        if let Some(message) = &self.restart_required {
            parts.push(format!("restart required: {message}"));
        }

        parts.join(" · ")
    }' \
  geode-shell set_restart_required_does_not_appear_in_the_summary

run_mutation "diagnostics: MAJ-5 — note_config appends instead of replacing" \
  crates/geode-shell/src/diagnostics.rs \
  '    pub fn note_config(&mut self, diags: Vec<Diagnostic>, at: SystemTime) {
        if self.config == diags {
            return;
        }
        self.config = diags.clone();
        self.config_history.push_front((at, diags));' \
  '    pub fn note_config(&mut self, diags: Vec<Diagnostic>, at: SystemTime) {
        if diags.is_empty() {
            return;
        }
        self.config.extend(diags.clone());
        self.config_history.push_front((at, diags));' \
  geode-shell note_config_is_a_no_op_for_an_identical_batch

run_mutation "diagnostics: MAJ-1 — summary() rebuilds the Rc<str> on a cache hit" \
  crates/geode-shell/src/diagnostics.rs \
  '        {
            let cache = self.summary_cache.borrow();
            if cache.0 == self.version {
                return cache.1.clone();
            }
        }' \
  '        {
            let cache = self.summary_cache.borrow();
            if false {
                return cache.1.clone();
            }
        }' \
  geode-shell summary_reuses_the_same_allocation_when_the_version_is_unchanged

run_mutation "diagnostics: MIN-2 — describe_source bumps for an identical summary" \
  crates/geode-shell/src/diagnostics.rs \
  '        let state = self.sources.entry(source.to_string()).or_default();
        if state.spec.as_ref() == Some(&summary) {
            return;
        }
        state.spec = Some(summary);' \
  '        let state = self.sources.entry(source.to_string()).or_default();
        if false {
            return;
        }
        state.spec = Some(summary);' \
  geode-shell describe_source_is_a_no_op_for_an_identical_summary

run_mutation "diagnostics: MIN-3 — request_level re-queues a persist when unchanged" \
  crates/geode-shell/src/diagnostics.rs \
  '        if self
            .levels
            .targets
            .iter()
            .any(|(t, l)| t == target && *l == level)
        {
            return;
        }
        self.levels = self.levels.with(target, level);' \
  '        if false {
            return;
        }
        self.levels = self.levels.with(target, level);' \
  geode-shell request_level_is_a_no_op_when_the_target_already_has_that_level

run_mutation "diagnostics: MIN-4 — unwatch never clears a pending catalog request" \
  crates/geode-shell/src/diagnostics.rs \
  '    pub fn unwatch(&mut self) {
        self.watchers = self.watchers.saturating_sub(1);
        if self.watchers == 0 {
            self.pending_catalog_request = false;
        }
    }' \
  '    pub fn unwatch(&mut self) {
        self.watchers = self.watchers.saturating_sub(1);
    }' \
  geode-shell unwatch_to_zero_clears_a_pending_catalog_request

run_mutation "diagnostics: MIN-5 — set_catalog keeps a dataset missing from a newer snapshot" \
  crates/geode-shell/src/diagnostics.rs \
  '        for state in self.datasets.values_mut() {
            state.catalog = None;
        }
        for ds in &snapshot.datasets {' \
  '        for ds in &snapshot.datasets {' \
  geode-shell set_catalog_drops_a_dataset_missing_from_a_newer_snapshot

run_mutation "hot_reload: MIN-9 — set_levels guarded behind LogServices too" \
  crates/geode-shell/src/shell/hot_reload.rs \
  '                if new_levels != self.diagnostics.read(cx).levels {
                    if let Some(log) = &self.services.log
                        && let Err(e) = log.control.set(&new_levels)
                    {
                        tracing::warn!(target: "geode::config", "failed to apply [log]: {e}");
                    }
                    self.diagnostics.update(cx, |d, cx| {
                        if d.set_levels(new_levels) {
                            cx.notify();
                        }
                    });
                }' \
  '                if let Some(log) = &self.services.log
                    && new_levels != self.diagnostics.read(cx).levels
                {
                    if let Err(e) = log.control.set(&new_levels) {
                        tracing::warn!(target: "geode::config", "failed to apply [log]: {e}");
                    }
                    self.diagnostics.update(cx, |d, cx| {
                        if d.set_levels(new_levels) {
                            cx.notify();
                        }
                    });
                }' \
  geode-shell a_log_table_change_updates_the_entity_even_without_log_services

run_mutation "log: MIN-10 — LogLevels.targets not canonicalised by from_doc" \
  crates/geode-core/src/log/mod.rs \
  '        levels.targets.sort();
        (levels, diags)
    }' \
  '        let _ = &levels.targets;
        (levels, diags)
    }' \
  geode-core targets_in_a_different_file_order_compare_equal

# MAJ-6: both stated deviations get a harness entry.
run_mutation "hot_reload: MAJ-6 — the reload path persists through request_level, not set_levels" \
  crates/geode-shell/src/shell/hot_reload.rs \
  '                    self.diagnostics.update(cx, |d, cx| {
                        if d.set_levels(new_levels) {
                            cx.notify();
                        }
                    });
                }' \
  '                    self.diagnostics.update(cx, |d, cx| {
                        d.request_level("ingest", geode_core::log::Level::DEBUG);
                        cx.notify();
                    });
                }' \
  geode-shell a_log_table_change_on_reload_applies_it_through_level_control_once

run_mutation "diagnostics: MAJ-6 — watch() no longer requests the first catalog" \
  crates/geode-shell/src/diagnostics.rs \
  '        self.pending_catalog_request = true;
        true
    }' \
  '        false
    }' \
  geode-shell watching_itself_also_requests_the_first_catalog

run_mutation "diagnostics: MIN-8 — CONFIG_HISTORY_CAP loosened from 16" \
  crates/geode-shell/src/diagnostics.rs \
  'pub const CONFIG_HISTORY_CAP: usize = 16;' \
  'pub const CONFIG_HISTORY_CAP: usize = 1600;' \
  geode-shell config_history_is_capped_at_sixteen_batches

# --- Task 4 fix round 2: NEW-1 (config/data diagnostics split) -------

run_mutation "diagnostics: NEW-1 — note_config also clears data_diagnostics" \
  crates/geode-shell/src/diagnostics.rs \
  '        self.config = diags.clone();
        self.config_history.push_front((at, diags));' \
  '        self.config = diags.clone();
        self.data_diagnostics.clear();
        self.config_history.push_front((at, diags));' \
  geode-shell a_config_reload_does_not_clobber_a_standing_data_diagnostic

run_mutation "diagnostics: NEW-1 — note_data_diagnostics also clears config" \
  crates/geode-shell/src/diagnostics.rs \
  '    pub fn note_data_diagnostics(&mut self, diags: Vec<Diagnostic>, at: SystemTime) {
        let mut changed = false;' \
  '    pub fn note_data_diagnostics(&mut self, diags: Vec<Diagnostic>, at: SystemTime) {
        self.config.clear();
        let mut changed = false;' \
  geode-shell a_data_diagnostic_does_not_clobber_a_standing_config_error

run_mutation "diagnostics: NEW-1 — note_data_diagnostics re-appends an identical entry" \
  crates/geode-shell/src/diagnostics.rs \
  '        for d in diags {
            if self
                .data_diagnostics
                .iter()
                .any(|(_, existing)| existing == &d)
            {
                continue;
            }
            self.data_diagnostics.push_back((at, d));
            changed = true;
        }' \
  '        for d in diags {
            self.data_diagnostics.push_back((at, d));
            changed = true;
        }' \
  geode-shell note_data_diagnostics_does_not_reappend_an_identical_entry

run_mutation "diagnostics: NEW-1 — DATA_DIAGNOSTICS_CAP loosened from 256" \
  crates/geode-shell/src/diagnostics.rs \
  'pub const DATA_DIAGNOSTICS_CAP: usize = 256;' \
  'pub const DATA_DIAGNOSTICS_CAP: usize = 25600;' \
  geode-shell data_diagnostics_is_capped_at_two_hundred_fifty_six

run_mutation "diagnostics: NEW-1 — summary omits the data error count" \
  crates/geode-shell/src/diagnostics.rs \
  '        if data_errors > 0 {
            parts.push(format!("data {data_errors} error{}", plural(data_errors)));
        }' \
  '' \
  geode-shell a_config_reload_does_not_clobber_a_standing_data_diagnostic

# --- Task 5: the geode-diagnostics module ---------------------------

run_mutation "diagnostics module: sources sorted best-first instead of worst-first" \
  crates/geode-diagnostics/src/sections.rs \
  '    reported.sort_by(|a, b| b.1.health.cmp(&a.1.health).then_with(|| a.0.cmp(b.0)));' \
  '    reported.sort_by(|a, b| a.1.health.cmp(&b.1.health).then_with(|| a.0.cmp(b.0)));' \
  geode-diagnostics sources_are_sorted_worst_first_with_their_detail

run_mutation "diagnostics module: the resolved-generation marker points at the wrong generation" \
  crates/geode-diagnostics/src/sections.rs \
  '                    && part.resolved_gen == Some(generation.gen_id);' \
  '                    && part.resolved_gen == Some(generation.gen_id + 1);' \
  geode-diagnostics data_rows_mark_the_resolved_generation_under_an_as_of

run_mutation "diagnostics module: the log filter matches every row regardless of target or level" \
  crates/geode-diagnostics/src/sections.rs \
  '        .filter(|r| filter.is_empty() || r.text.contains(filter))
        .collect()' \
  '        .filter(|_r| true)
        .collect()' \
  geode-diagnostics log_rows_filter_by_target_or_level_text

run_mutation "diagnostics module: move_cursor never clears follow" \
  crates/geode-diagnostics/src/tile.rs \
  '        self.cursor = target as usize;
        self.follow = false;' \
  '        self.cursor = target as usize;' \
  geode-diagnostics the_log_section_follows_the_tail_until_the_cursor_moves

run_mutation "diagnostics module: the diagnostics observer rebuilds on every notify, not just a real version change" \
  crates/geode-diagnostics/src/tile.rs \
  '            this.last_diag_versions = now;
            if relevant {
                this.rebuild(cx);
            }
        })
        .detach();
        // MIN-7 (final review)' \
  '            this.last_diag_versions = now;
            this.rebuild(cx);
        })
        .detach();
        // MIN-7 (final review)' \
  geode-diagnostics an_unchanged_entity_does_not_rebuild_rows

# Re-homed 2026-09-08 (add-tile): `open_module` moved from `shell/mod.rs`
# into `shell/add_tile.rs` beside the one door it now goes through.
run_mutation "shell: open_module never finds an existing occupant, so a second call re-opens a second tile" \
  crates/geode-shell/src/shell/add_tile.rs \
  '        let ws = self.services.workspaces.active();
        let found: Option<(TileId, Option<DockSide>)> = ws
            .tree()
            .tiles()
            .into_iter()
            .find(|id| self.occupant_kind(*id) == Some(kind))
            .map(|id| (id, None))
            .or_else(|| {
                ws.docks().iter().find_map(|(side, dock)| {
                    dock.tree()
                        .tiles()
                        .into_iter()
                        .find(|id| self.occupant_kind(*id) == Some(kind))
                        .map(|id| (id, Some(side)))
                })
            });' \
  '        let found: Option<(TileId, Option<DockSide>)> = None;' \
  geode-shell open_module_twice_yields_one_tile_of_that_kind_focused

run_mutation "diagnostics module: the log section never reports records lost to a ring wrap" \
  crates/geode-diagnostics/src/tile.rs \
  '                self.lost_records = self
                    .ring
                    .oldest_seq()
                    .map(|oldest| oldest.saturating_sub(self.since + 1))
                    .unwrap_or(0);' \
  '                self.lost_records = 0;' \
  geode-diagnostics the_log_section_reports_lost_records_when_the_ring_wrapped_past_since

# --- Task 5 fix round 1 ----------------------------------------------

run_mutation "diagnostics module: MAJ-1 — sync_scroll never scrolls the list" \
  crates/geode-diagnostics/src/tile.rs \
  '    fn sync_scroll(&self) {
        self.scroll
            .scroll_to_item(self.cursor, ScrollStrategy::Nearest);
    }' \
  '    fn sync_scroll(&self) {}' \
  geode-diagnostics pressing_bottom_scrolls_the_list_to_the_last_row

run_mutation "shell: MAJ-2 — ensure_occupants drops a vanished tile's occupant without unwatching it" \
  crates/geode-shell/src/shell/occupants.rs \
  '        for (id, o) in self.occupants.iter() {
            if !all.contains(id) {
                o.content.set_visible(false, cx);
            }
        }
        self.occupants.retain(|id, _| all.contains(id));' \
  '        self.occupants.retain(|id, _| all.contains(id));' \
  geode-shell closing_a_watching_tile_unwatches_the_diagnostics_entity

run_mutation "shell: MAJ-3 — note_config_reloaded goes back behind the views_changed gate" \
  crates/geode-shell/src/shell/hot_reload.rs \
  '            {
                self.frame.update(cx, |f, cx| {
                    f.note_config_reloaded();' \
  '            if views_changed {
                self.frame.update(cx, |f, cx| {
                    f.note_config_reloaded();' \
  geode-shell a_reload_that_does_not_touch_views_or_dimensions_still_bumps_the_config_version

run_mutation "diagnostics module: MAJ-5 — a real log drain allocates a fresh buffer instead of reusing drain_buf" \
  crates/geode-diagnostics/src/tile.rs \
  '                self.ring.drain_since(self.since, &mut self.drain_buf);' \
  '                let mut fresh_drain_buf = Vec::new();
                self.ring.drain_since(self.since, &mut fresh_drain_buf);
                self.drain_buf = fresh_drain_buf;' \
  geode-diagnostics a_no_op_log_drain_does_not_grow_the_drain_buffer

run_mutation "diagnostics module: the frame observer rebuilds Sources/Log/Perf on an as_of-or-config change too (successor of the retired MAJ-6 frame_versions_relevant_eq entry — that function was deleted by MAJ-4)" \
  crates/geode-diagnostics/src/tile.rs \
  '                Section::Sources | Section::Log | Section::Perf => false,' \
  '                Section::Sources | Section::Log | Section::Perf => as_of_changed || config_changed,' \
  geode-diagnostics a_config_reload_while_showing_sources_does_not_rebuild

run_mutation "diagnostics module: MAJ-7 — an as-of change while visible never requests a fresh catalog" \
  crates/geode-diagnostics/src/tile.rs \
  '            if as_of_changed && this.visible {
                this.diagnostics.update(cx, |d, cx| {
                    d.request_catalog();
                    cx.notify();
                });
            }' \
  '            if false {
                this.diagnostics.update(cx, |d, cx| {
                    d.request_catalog();
                    cx.notify();
                });
            }' \
  geode-diagnostics an_as_of_change_while_visible_requests_a_fresh_catalog

run_mutation "diagnostics module: MAJ-8 — the config explainer stops recursing into arrays" \
  crates/geode-diagnostics/src/sections.rs \
  '        toml::Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                walk_value(v, &format!("{path}.{i}"), out);
            }
        }
        other => out.push((path.to_string(), other.to_string())),' \
  '        other => out.push((path.to_string(), other.to_string())),' \
  geode-diagnostics config_rows_recurses_into_arrays_with_indexed_paths

run_mutation "diagnostics module: MIN-4 — page_down/page_up drop the count multiplier" \
  crates/geode-diagnostics/src/tile.rs \
  '            "page_down" => self.move_cursor(5 * n, cx),
            "page_up" => self.move_cursor(-5 * n, cx),' \
  '            "page_down" => self.move_cursor(5, cx),
            "page_up" => self.move_cursor(-5, cx),' \
  geode-diagnostics a_count_prefix_multiplies_page_down

run_mutation "diagnostics module: ctrl+f/ctrl+b page by ten, not by five" \
  crates/geode-diagnostics/src/tile.rs \
  '            "page_down_full" => self.move_cursor(10 * n, cx),
            "page_up_full" => self.move_cursor(-10 * n, cx),' \
  '            "page_down_full" => self.move_cursor(5 * n, cx),
            "page_up_full" => self.move_cursor(-5 * n, cx),' \
  geode-diagnostics ctrl_f_and_ctrl_b_page_by_ten

run_mutation "blotter: ctrl+b moves back ten, not forward" \
  crates/geode-blotter/src/tile.rs \
  '                    "page_down_full" => NavCommand::Move(10),
                    _ => NavCommand::Move(-10),' \
  '                    "page_down_full" => NavCommand::Move(10),
                    _ => NavCommand::Move(10),' \
  geode-blotter motions_expansion_and_yank

run_mutation "line numbers: the relative cursor row shows its absolute number, not 0" \
  crates/geode-shell/src/linenumbers.rs \
  '        LineNumbers::Relative if row == cursor => Some(row + 1),' \
  '        LineNumbers::Relative if row == cursor => Some(0),' \
  geode-shell relative_shows_the_absolute_number_on_the_cursor_row

run_mutation "line numbers: relative is a distance, not a signed offset" \
  crates/geode-shell/src/linenumbers.rs \
  '        LineNumbers::Relative => Some(row.abs_diff(cursor)),' \
  '        LineNumbers::Relative => Some(row.saturating_sub(cursor)),' \
  geode-shell relative_is_the_distance_from_the_cursor_in_both_directions

run_mutation "blotter gutter: a rel cursor move re-derives the numbers (stamp carries the cursor)" \
  crates/geode-blotter/src/delegate.rs \
  '            LineNumbers::Relative => cursor,
            _ => usize::MAX,' \
  '            LineNumbers::Relative => usize::MAX,
            _ => usize::MAX,' \
  geode-blotter the_gutter_follows_the_mode_and_the_cursor

run_mutation "blotter gutter: the stamp compares, not merely exists" \
  crates/geode-blotter/src/delegate.rs \
  '        if self.numbers_stamp.as_ref() == Some(&stamp) {' \
  '        if self.numbers_stamp.is_some() {' \
  geode-blotter the_gutter_follows_the_mode_and_the_cursor

run_mutation "blotter: the tree column is pinned left while the measures scroll" \
  crates/geode-blotter/src/delegate.rs \
  '            fixed: if c.kind == ColumnKind::Tree {
                Some(ColumnFixed::Left)
            } else {
                None
            },' \
  '            fixed: None,' \
  geode-blotter the_tree_column_stays_put_when_the_table_scrolls_right

run_mutation "blotter gutter: the tree column widens by the gutter" \
  crates/geode-blotter/src/delegate.rs \
  '            width: px(if c.kind == ColumnKind::Tree {
                c.width + self.gutter_px()
            } else {
                c.width
            }),' \
  '            width: px(c.width),' \
  geode-blotter the_line_numbers_global_paints_a_gutter_on_the_next_draw

run_mutation "blotter gutter: a changed setting refreshes the table's column groups" \
  crates/geode-blotter/src/tile.rs \
  '        if changed {
            self.table.update(cx, |t, cx| {
                t.refresh(cx);
                cx.notify();
            });
            cx.notify();
        }' \
  '        if changed {
            cx.notify();
        }' \
  geode-blotter the_line_numbers_global_paints_a_gutter_on_the_next_draw

run_mutation "diagnostics module: MIN-5 — set_visible(false) unwatches but never notifies" \
  crates/geode-diagnostics/src/tile.rs \
  '            self.diagnostics.update(cx, |d, cx| {
                d.unwatch();
                cx.notify();
            });
        }
    }' \
  '            self.diagnostics.update(cx, |d, _cx| {
                d.unwatch();
            });
        }
    }' \
  geode-diagnostics set_visible_false_unwatches_and_notifies

# Re-homed and re-anchored 2026-09-08 (add-tile): `open_module` moved to
# `shell/add_tile.rs`, the guard now scans the addressed `pending_tiles`
# map rather than a single `pending_kind_for_new_tile`, and its test was
# renamed `..._add_only_once` (it is an add, not a split).
run_mutation "shell: MIN-7 — open_module loses its same-pending-kind guard" \
  crates/geode-shell/src/shell/add_tile.rs \
  '        if self.pending_tiles.values().any(|p| p.kind == kind) {
            return;
        }
        self.add_tile(kind, None, None, window, cx);' \
  '        self.add_tile(kind, None, None, window, cx);' \
  geode-shell two_open_module_calls_for_the_same_kind_before_any_render_add_only_once

run_mutation "diagnostics module: MIN-11 — the header never shows the filtered pill" \
  crates/geode-diagnostics/src/tile.rs \
  '        if !self.filter.is_empty() {
            header = header.child(
                div()
                    .text_color(theme.warning_foreground)' \
  '        if false {
            header = header.child(
                div()
                    .text_color(theme.warning_foreground)' \
  geode-diagnostics a_filtered_tile_shows_the_filtered_pill

# --- Task 5 fix round 2 ----------------------------------------------

run_mutation "diagnostics module: MAJ-7 — an as-of change while visible never requests a fresh catalog (bridge drain, end to end)" \
  crates/geode-diagnostics/src/tile.rs \
  '            if as_of_changed && this.visible {' \
  '            if false {' \
  geode-app an_as_of_change_on_a_visible_diagnostics_tile_requests_a_second_catalog_with_the_new_as_of

# ---- Phase 4b Task 6: the panic boundaries (the ingest error event, ----
# ---- the crash file, the action tail) -----------------------------------

run_mutation "ingest runner: the panic payload is dropped from the reported Failed.reason" \
  crates/geode-data/src/ingest/runner.rs \
  '                    reason: format!("ingest task panicked at {path}: {message}"),' \
  '                    reason: format!("ingest task panicked at {path}"),' \
  geode-data a_panicking_load_names_the_file_and_the_panic_payload

run_mutation "crash file: the log ring's records are dropped from write_crash_file's output" \
  crates/geode-app/src/crash.rs \
  '    for r in records {
        out.push_str(&format_record(r));' \
  '    for r in records.iter().take(0) {
        out.push_str(&format_record(r));' \
  geode-app write_crash_file_contains_the_message_location_records_and_actions_in_order

run_mutation "shell: dispatch never records the dispatched action into the tail" \
  crates/geode-shell/src/shell/input.rs \
  '        self.services
            .action_tail
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record(&action.0);' \
  '        let _ = &action.0;' \
  geode-shell dispatching_three_actions_leaves_their_hashes_in_the_tail_in_order

run_mutation "trim_log_files keeps one file more than asked (keep + 1, not keep)" \
  crates/geode-app/src/crash.rs \
  '    for old in &files[..files.len() - keep] {' \
  '    for old in &files[..files.len() - keep - 1] {' \
  geode-app trim_deletes_the_oldest_files_beyond_the_cap

# ---- Task 6 fix round 1: panic containment (MAJ-1), the runner's -------
# ---- panic log extracted and tested (MAJ-2) -----------------------------

run_mutation "geode_core::panic: contained never marks the thread as inside a boundary" \
  crates/geode-core/src/panic.rs \
  '    DEPTH.with(|d| d.set(d.get() + 1));' \
  '    DEPTH.with(|d| d.set(d.get()));' \
  geode-core contained_reports_true_only_for_its_own_extent

run_mutation "geode_core::panic: the guard's drop never clears the marker" \
  crates/geode-core/src/panic.rs \
  '            DEPTH.with(|d| d.set(d.get().saturating_sub(1)));' \
  '            let _ = DEPTH.with(|d| d.get());' \
  geode-core contained_reports_true_only_for_its_own_extent

run_mutation "ingest runner: log_ingest_panic never logs (its error! call is dropped)" \
  crates/geode-data/src/ingest/runner.rs \
  '    tracing::error!(target: "geode::ingest", file = %path.display(), "ingest task panicked: {message}");' \
  '    let _ = (path, message);' \
  geode-data log_ingest_panic_logs_the_file_and_payload_at_error

run_mutation "crash file: write_crash_file truncates a same-instant collision instead of suffixing it" \
  crates/geode-app/src/crash.rs \
  '.create_new(true)' \
  '.create(true).truncate(true)' \
  geode-app a_second_write_at_the_same_instant_gets_a_suffixed_name_not_a_truncation

run_mutation "crash file: write_crash_file never prunes old crash-*.log files" \
  crates/geode-app/src/crash.rs \
  '    prune_files(dir, "crash-", ".log", CRASH_FILES_KEPT);
    Ok(path)' \
  '    Ok(path)' \
  geode-app write_crash_file_prunes_to_the_newest_ten

# ---- Final fix wave (whole-branch review, 2026-09-08): MAJ-1..MAJ-4 ----

run_mutation "service: an ingest failure is keyed by the source name, not the dataset" \
  crates/geode-data/src/service.rs \
  '                            Some((worst, detail)) => sink(DataEvent::Health {
                                source: source.clone(),
                                worst,
                                detail,
                            }),' \
  '                            Some((worst, detail)) => sink(DataEvent::Health {
                                source: dataset.clone(),
                                worst,
                                detail,
                            }),' \
  geode-data a_load_failure_reports_health_under_the_source_name_not_the_dataset_name

run_mutation "service: a degraded publish also reaches the entity as Health" \
  crates/geode-data/src/service.rs \
  '                        format!("{batch}: {reason}"),
                        |reported| match reported {
                            Some((worst, detail)) => {
                                log_health_event(&source, &worst, &detail);
                                sink(DataEvent::Health {
                                    source: source.clone(),
                                    worst,
                                    detail,
                                })
                            }
                            None => true,
                        },' \
  '                        format!("{batch}: {reason}"),
                        |reported| match reported {
                            Some((worst, detail)) => {
                                log_health_event(&source, &worst, &detail);
                                true
                            }
                            None => true,
                        },' \
  geode-data a_degraded_publish_reaches_the_entity_as_degraded_health

run_mutation "scheduler: a clean poll always sends Health::Ok now (dedup moved to DataService's shared HealthTracker)" \
  crates/geode-data/src/ingest/scheduler.rs \
  '                        None => sink(SchedulerEvent::Health {
                            source: spec.name.clone(),
                            worst: Health::Ok,
                            detail: String::new(),
                        }),' \
  '                        None => true,' \
  geode-data a_steadily_healthy_source_reports_ok_on_every_poll

# ---- final review round 2: NEW-1 (one HealthTracker, shared by both sinks) ----

run_mutation "service: HealthTracker.report returns Some unconditionally, never deduping" \
  crates/geode-data/src/service.rs \
  '        if combined == self.last_reported {
            return emit(None);
        }' \
  '        if false {
            return emit(None);
        }' \
  geode-data a_clean_scheduler_poll_and_a_clean_publish_together_send_exactly_one_ok

run_mutation "service: the ingest sink skips reporting a clean (Ok) publish to the shared tracker" \
  crates/geode-data/src/service.rs \
  '                    let reason = match &health {
                        Health::Degraded { reason } | Health::Failed { reason } => reason.clone(),
                        _ => String::new(),
                    };' \
  '                    let reason = match &health {
                        Health::Degraded { reason } | Health::Failed { reason } => reason.clone(),
                        _ => String::new(),
                    };
                    if health == Health::Ok {
                        return delivered;
                    }' \
  geode-data a_clean_republish_of_the_same_batch_clears_its_degraded_health

# ---- final review round 3: NEW-4 (two health lanes, combined as the worse) ----

run_mutation "service: HealthTracker.report combines by taking the discovery lane instead of the worse of the two" \
  crates/geode-data/src/service.rs \
  '        worse_of(self.discovery.as_ref(), load).map(|v| match &v.health {' \
  '        self.discovery.as_ref().or(load).map(|v| match &v.health {' \
  geode-data a_degraded_load_survives_a_clean_discovery_poll

run_mutation "service: the ingest sink never writes the load lane, so a publish never affects the tracker" \
  crates/geode-data/src/service.rs \
  '                    let health_delivered = health_tracker.report_load_and_emit(
                        &source,
                        &batch,
                        health,
                        format!("{batch}: {reason}"),
                        |reported| match reported {
                            Some((worst, detail)) => {
                                log_health_event(&source, &worst, &detail);
                                sink(DataEvent::Health {
                                    source: source.clone(),
                                    worst,
                                    detail,
                                })
                            }
                            None => true,
                        },
                    );' \
  '                    let health_delivered = {
                        let _ = (&source, &batch, &health, &reason, &sink);
                        true
                    };' \
  geode-data a_degraded_publish_reaches_the_entity_as_degraded_health

# ---- final review round 4: NEW-5 (severity rank, the deciding lane's detail)
#      and NEW-6 (the load lane keyed per batch) ----

run_mutation "service: lanes are compared by Health's derived Ord (reason TEXT) instead of severity rank" \
  crates/geode-data/src/service.rs \
  'fn displaces(candidate: &LaneValue, incumbent: &LaneValue) -> bool {
    (severity_rank(&candidate.health), candidate.changed)
        > (severity_rank(&incumbent.health), incumbent.changed)
}' \
  'fn displaces(candidate: &LaneValue, incumbent: &LaneValue) -> bool {
    (candidate.health.clone(), candidate.changed) > (incumbent.health.clone(), incumbent.changed)
}' \
  geode-data a_second_degradation_at_the_same_rank_is_reported

run_mutation "service: an identical re-report restamps its slot, so the calling lane wins every tie" \
  crates/geode-data/src/service.rs \
  'fn unchanged_stamp(slot: Option<&LaneValue>, health: &Health, detail: &str) -> Option<u64> {
    slot.filter(|v| v.health == *health && v.detail == detail)
        .map(|v| v.changed)
}' \
  'fn unchanged_stamp(slot: Option<&LaneValue>, health: &Health, detail: &str) -> Option<u64> {
    let _ = (slot, health, detail);
    None
}' \
  geode-data repeated_identical_polls_at_the_same_rank_do_not_flap_the_decision

# `lanes.offer(emit)` ends both doors, so the anchor carries the
# discovery lane's own write above it -- the load door writes
# `lanes.load.insert(...)` instead. Anchoring on the bare line relied on
# `replace(..., 1)` happening to reach the discovery door first, which is
# exactly the silent-flip this pass exists to remove. The mutation writes
# the updated `Lanes` back afterwards, so it breaks ONLY the lock-holding,
# not the bookkeeping (a mutation that also dropped the commit would be
# caught by half the module for reasons that have nothing to do with its
# name).
run_mutation "service: the discovery door emits after dropping the tracker lock, so two reporters can reorder" \
  crates/geode-data/src/service.rs \
  '        lanes.discovery = Some(LaneValue {
            health,
            detail,
            changed,
        });
        lanes.offer(emit)' \
  '        lanes.discovery = Some(LaneValue {
            health,
            detail,
            changed,
        });
        let mut detached = lanes.clone();
        drop(sources);
        let out = detached.offer(emit);
        self.sources
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(source.to_string(), detached);
        out' \
  geode-data emit_runs_with_the_tracker_lock_held

# ---- final review round 5: finding 2 (commit only what was delivered) ----

run_mutation "service: a health transition is recorded as reported even when its event was refused" \
  crates/geode-data/src/service.rs \
  '        let delivered = emit(combined.clone());
        if delivered {
            self.last_reported = combined;
        }
        delivered' \
  '        let delivered = emit(combined.clone());
        self.last_reported = combined;
        delivered' \
  geode-data a_refused_health_event_is_offered_again_not_recorded_as_reported

run_mutation "service: the load lane is keyed by source only, so any batch's clean publish clears every other" \
  crates/geode-data/src/service.rs \
  '        let kept = unchanged_stamp(lanes.load.get(batch), &health, &detail);' \
  '        let batch = "";
        let kept = unchanged_stamp(lanes.load.get(batch), &health, &detail);' \
  geode-data a_clean_publish_of_one_batch_leaves_another_batchs_degraded_standing

run_mutation "diagnostics tile: the diagnostics observer compares the current section's version, not just any version" \
  crates/geode-diagnostics/src/tile.rs \
  '                diag_version_for_section(this.section, now)
                    != diag_version_for_section(this.section, this.last_diag_versions)' \
  '                true' \
  geode-diagnostics refresh_frame_hist_does_not_rebuild_the_config_section

run_mutation "sections: the perf section shows database/memory bytes and threads once a catalog arrives" \
  crates/geode-diagnostics/src/sections.rs \
  '                "database {} (checkpointed) · memory {} · threads {}",' \
  '                "db {} (checkpointed) · mem {} · thr {}",' \
  geode-diagnostics perf_rows_show_database_bytes_memory_bytes_and_threads_once_a_catalog_arrives

run_mutation "diagnostics tile: set_section replaces the cached header text" \
  crates/geode-diagnostics/src/tile.rs \
  '        self.header_text = header_text_for(section);' \
  '        let _ = header_text_for(section);' \
  geode-diagnostics the_header_text_is_cached_across_paints_and_replaced_on_section_change

run_mutation "diagnostics tile: since is seeded from the ring's current latest_seq, not 0" \
  crates/geode-diagnostics/src/tile.rs \
  '            since: initial_since,' \
  '            since: 0,' \
  geode-diagnostics a_freshly_opened_tile_does_not_claim_records_it_never_had

run_mutation "sections: the resolved-generation marker requires the snapshot's own as_of to match the frame's" \
  crates/geode-diagnostics/src/sections.rs \
  '                let marked = !as_of.is_live()
                    && snapshot_matches_as_of
                    && part.resolved_gen == Some(generation.gen_id);' \
  '                let marked = !as_of.is_live()
                    && part.resolved_gen == Some(generation.gen_id);' \
  geode-diagnostics data_rows_suppresses_the_marker_when_the_snapshot_as_of_does_not_match_the_frames

# Retired 2026-09-08 (add-tile): this guarded "a single
# `pending_kind_for_new_tile` is spent on the LOWEST TileId when two
# tiles go occupant-less in one pass", and both halves are gone — the
# request map is addressed by `TileId` now (spec §4.3), so no ordering
# decides who gets it, and the test it named went with the mechanism.
# `creation_order.sort()` survives, but only to keep the order factories
# are constructed in stable; nothing observable turns on it, so an entry
# here would be one with no test behind it. What replaced this defence
# is "add-tile: the pending request is keyed by the id split_active
# returned" in the add-tile block below.

run_mutation "sections: a source's path, priority and readiness are separate rows, not one long one" \
  crates/geode-diagnostics/src/sections.rs \
  '    out.push(row(format!("path: {paths}"), 1, Tone::Muted));
    out.push(row(
        format!(
            "priority: {} · readiness: {}",
            spec.priority, spec.readiness
        ),
        1,
        Tone::Muted,
    ));' \
  '    out.push(row(
        format!("path: {paths} · priority: {} · readiness: {}", spec.priority, spec.readiness),
        1,
        Tone::Muted,
    ));' \
  geode-diagnostics a_sources_spec_detail_is_split_into_short_rows

run_mutation "commands: a diagnostics completion is the word under the cursor, not the whole line" \
  crates/geode-diagnostics/src/commands.rs \
  '        ["level"] => known_targets().map(str::to_string).collect(),' \
  '        ["level"] => known_targets().map(|t| format!("level {t}")).collect(),' \
  geode-diagnostics a_candidate_is_the_word_under_the_cursor_not_the_line

run_mutation "commands: diagnostics completions split words on the shell's delimiters, not just a space" \
  crates/geode-diagnostics/src/commands.rs \
  '        .split(|c: char| c.is_whitespace() || c == '"'"','"'"')' \
  '        .split('"'"' '"'"')' \
  geode-diagnostics completions_split_words_the_way_the_shell_does

# ---- 2026-09-08 add-tile (spec 2026-09-08-geode-add-tile-design.md)
#
# The split verbs are gone: a tile is *added* by kind, and the split is
# only how the add is placed. Nearly every behaviour below is silent when
# wrong — a tile that lands on the wrong side, a request that fills the
# wrong tile, a session record quietly dropped on the next flush — so
# each one gets an entry.

run_mutation "add-tile: auto splits along the longer side (w >= h → right)" \
  crates/geode-shell/src/tileadd.rs \
  '                Some(r) if r.h > r.w => Orientation::Vertical,' \
  '                Some(r) if r.h < r.w => Orientation::Vertical,' \
  geode-shell auto_splits_along_the_longer_side_and_a_square_or_missing_rect_goes_right

run_mutation "add-tile: an explicit direction beats the setting" \
  crates/geode-shell/src/tileadd.rs \
  '        if let Some(o) = explicit {' \
  '        if let Some(o) = explicit.filter(|_| false) {' \
  geode-shell an_explicit_direction_beats_the_setting

run_mutation "add-tile: a focused placeholder is filled in place, not split" \
  crates/geode-shell/src/shell/add_tile.rs \
  '            && self.occupant_kind(tile) == Some(PLACEHOLDER_KIND)' \
  '            && self.occupant_kind(tile) == Some("never")' \
  geode-shell add_on_a_placeholder_tile_fills_it_in_place

run_mutation "add-tile: the pending request is keyed by the id split_active returned" \
  crates/geode-shell/src/tiling/workspaces.rs \
  '        id
    }' \
  '        TileId(id.0 + 1)
    }' \
  geode-shell a_pending_request_lands_on_exactly_the_tile_that_asked

run_mutation "add-tile: duplicate carries the focused tile's serialized state" \
  crates/geode-shell/src/shell/add_tile.rs \
  '        self.add_tile(&kind, Some(direction), Some(state), window, cx);' \
  '        let _ = state; self.add_tile(&kind, Some(direction), None, window, cx);' \
  geode-shell shift_d_duplicates_the_focused_tile_with_its_state_and_ctrl_shift_d_stacks_it

# Two-line anchor on purpose (final review, Minor 2): the bare
# `self.enter_region(FocusRegion::Dock(side));` line occurs four times in
# this file (`toggle_dock`, both `move_to_dock` arms, the drag drop) and
# `run_mutation` replaces the FIRST match — the pairing with the
# preceding `exit_fullscreen()` is unique to `toggle_dock`'s show branch.
# The mutation keeps `exit_fullscreen()` and only makes showing NOT focus.
run_mutation "docks: showing a dock focuses it" \
  crates/geode-shell/src/tiling/workspaces.rs \
  '            self.tree.exit_fullscreen();
            self.enter_region(FocusRegion::Dock(side));' \
  '            self.tree.exit_fullscreen();
            self.docks.get_mut(side).set_visible(true);' \
  geode-shell toggling_a_hidden_dock_shows_it_and_focuses_it_even_when_empty

# Final review, Important 1: the cross-workspace restore pass must heal
# only a region naming a *hidden* dock. Healing on `focusable()` ("hidden
# OR empty") drags focus back to `Main` and prints a launch-time warning
# after the entirely ordinary "ctrl+[ on an empty left dock, then quit".
run_mutation "add-tile: a restored region survives an empty but visible dock" \
  crates/geode-shell/src/tiling/workspaces.rs \
  '                && !ws.docks.get(side).visible()' \
  '                && !ws.docks.get(side).focusable()' \
  geode-shell an_empty_but_visible_focused_dock_round_trips_without_a_warning

run_mutation "add-tile: an unknown restored kind keeps its record through a flush" \
  crates/geode-shell/src/shell/occupants.rs \
  '                self.unplaced_records.insert(id.0, record.clone());' \
  '                let _ = record;' \
  geode-shell a_restored_tile_of_an_unknown_kind_paints_the_placeholder_and_its_record_survives

run_mutation "add-tile: a pending request for a closed tile is dropped, not re-aimed" \
  crates/geode-shell/src/shell/occupants.rs \
  '        self.pending_tiles.retain(|id, p| {
            let live = all.contains(id);
            if !live {
                tracing::debug!(target: "geode::shell", "dropping a pending '"'"'{}'"'"' request for closed tile {}", p.kind, id.0);
            }
            live
        });' \
  '        self.pending_tiles.retain(|id, p| {
            let _ = (id, p);
            true
        });' \
  geode-shell a_pending_request_for_a_closed_tile_is_dropped_and_does_not_latch_open_module

run_mutation "add-tile: an unplaced record dies with its tile" \
  crates/geode-shell/src/shell/occupants.rs \
  '        self.unplaced_records
            .retain(|id, _| all.contains(&TileId(*id)));' \
  '        self.unplaced_records.retain(|id, _| {
            let _ = id;
            true
        });' \
  geode-shell closing_an_unknown_kind_tile_drops_its_unplaced_record

run_mutation "add-tile: filling a placeholder in place drops its unplaced record" \
  crates/geode-shell/src/shell/add_tile.rs \
  '            self.unplaced_records.remove(&tile.0);' \
  '            let _ = tile.0;' \
  geode-shell a_restored_tile_of_an_unknown_kind_paints_the_placeholder_and_its_record_survives

run_mutation "add-tile: the _vertical suffix means stacked" \
  crates/geode-shell/src/defaults.rs \
  '        (k, Some(Orientation::Vertical))' \
  '        (k, Some(Orientation::Horizontal))' \
  geode-shell parse_add_action_peels_the_direction_suffix_before_the_kind

run_mutation "add-tile: register_add_actions registers the suffixed pair too" \
  crates/geode-shell/src/defaults.rs \
  '            &format!("tile::add_{kind}_vertical"),' \
  '            &format!("tile::add_{kind}_vertical_"),' \
  geode-shell register_add_actions_registers_three_rows_per_kind_in_the_tiles_category

# ---- a refused event never stops a producer (Phase 4b follow-up, Task 1)

run_mutation "runner: a refused idle announcement drops the event, it does not stop the runner" \
  crates/geode-data/src/ingest/runner.rs \
  '                        drop(q);
                        log_refused_event(&refusal_logged, "the queue-drained announcement");
                        q = lock.lock().unwrap_or_else(|e| e.into_inner());
                        continue;' \
  '                        return;' \
  geode-data a_refused_plan_complete_does_not_stop_the_runner

run_mutation "runner: a refused undeclared-dataset failure does not stop the runner" \
  crates/geode-data/src/ingest/runner.rs \
  '            if !failed {
                log_refused_event(
                    &refusal_logged,
                    &format!(
                        "the undeclared-dataset failure for {}/{}",
                        item.dataset, item.batch
                    ),
                );
            }' \
  '            if !failed {
                return;
            }' \
  geode-data a_refused_undeclared_dataset_failure_does_not_stop_the_runner

run_mutation "runner: a refused load outcome does not stop the runner" \
  crates/geode-data/src/ingest/runner.rs \
  '        if !delivered {
            log_refused_event(
                &refusal_logged,
                &format!("the load outcome for {}/{}", item.dataset, item.batch),
            );
        }' \
  '        if !delivered {
            return;
        }' \
  geode-data a_refused_load_outcome_does_not_stop_the_runner

run_mutation "scheduler: a refused event does not stop polling every source" \
  crates/geode-data/src/ingest/scheduler.rs \
  '            log_refused_discovery(&refusal_logged, what, &spec.name);' \
  '            return;' \
  geode-data a_refused_event_does_not_stop_the_scheduler

run_mutation "pool: a refused result does not stop the worker" \
  crates/geode-data/src/query/pool.rs \
  '            log_refused_result(&refusal_logged, &req.view.0);' \
  '            return;' \
  geode-data a_refused_result_does_not_stop_the_worker

run_mutation "scheduler: a poll sends its result even when its health report was refused" \
  crates/geode-data/src/ingest/scheduler.rs \
  '                    let polled_delivered = sink(SchedulerEvent::Polled {' \
  '                    let polled_delivered = health_delivered
                        && sink(SchedulerEvent::Polled {' \
  geode-data a_refused_health_does_not_swallow_that_polls_result

run_mutation "bridge: a gone receiver is logged once per sink, not once per event" \
  crates/geode-app/src/bridge.rs \
  '            if err.is_closed() && !warned_closed.swap(true, Ordering::Relaxed) {' \
  '            if err.is_closed() && true {' \
  geode-app a_closed_channel_is_counted_and_logged_once

run_mutation "bridge: only a CLOSED channel is logged as a gone receiver, never a full one" \
  crates/geode-app/src/bridge.rs \
  '            if err.is_closed() && !warned_closed.swap(true, Ordering::Relaxed) {' \
  '            if !warned_closed.swap(true, Ordering::Relaxed) {' \
  geode-app a_full_channel_is_counted_but_not_reported_as_a_gone_receiver

run_mutation "service: open seeds the health load lane from the catalog" \
  crates/geode-data/src/service.rs \
  '        for dataset in datasets {
            let unhealthy = Catalog::new(&conn).live_health(dataset)?;' \
  '        for dataset in datasets.into_iter().take(0) {
            let unhealthy = Catalog::new(&conn).live_health(dataset)?;' \
  geode-data a_restart_seeds_the_load_lane_from_a_still_live_degraded_generation

run_mutation "service: the seed is filed under the key a publish writes" \
  crates/geode-data/src/service.rs \
  '                    health_tracker.report_load_and_emit(
                        &spec.name,
                        batch,' \
  '                    health_tracker.report_load_and_emit(
                        &spec.name,
                        &format!("{dataset}/{batch}"),' \
  geode-data a_seeded_batch_is_cleared_by_that_batchs_own_corrected_republish

run_mutation "catalog: live_health reports only generations that are not ok" \
  crates/geode-data/src/store/catalog.rs \
  "                         and health in ('failed', 'degraded', 'pending_too_long', 'pending')" \
  '                         and health is not null' \
  geode-data live_health_reports_only_the_batches_whose_live_generation_is_unhealthy

run_mutation "catalog: live_health admits only labels from_parts round-trips" \
  crates/geode-data/src/store/catalog.rs \
  "                         and health in ('failed', 'degraded', 'pending_too_long', 'pending')" \
  "                         and health <> 'ok'" \
  geode-data live_health_ignores_a_health_label_it_does_not_recognise

run_mutation "catalog: live_health breaks a tied source time on the newer generation" \
  crates/geode-data/src/store/catalog.rs \
  '                                      order by g.source_time desc, g.gen_id desc' \
  '                                      order by g.source_time desc, g.gen_id asc' \
  geode-data live_health_breaks_a_tied_source_time_on_the_newer_generation

run_mutation "catalog: live_health never picks an archived-only generation" \
  crates/geode-data/src/store/catalog.rs \
  '                            and coalesce(fg.archived_only, false) = false
                           where g.dataset = ?' \
  '                            and 1 = 1
                           where g.dataset = ?' \
  geode-data live_health_never_reads_an_archived_only_generation_as_live

run_mutation "catalog: live_health rolls a batch up to its worst book" \
  crates/geode-data/src/store/catalog.rs \
  "                                           end desc,
                                           source_time desc, gen_id desc" \
  "                                           end asc,
                                           source_time desc, gen_id desc" \
  geode-data live_health_takes_the_worst_across_the_books_of_one_batch

# ---- health follow-ups (Task 3): worst_health names both Orphaned files

run_mutation "scheduler: worst_health compares by rank, not Health's derived Ord" \
  crates/geode-data/src/ingest/scheduler.rs \
  '        let incumbent_rank = worst.first().map(|(w, _)| severity_rank(w));
        match incumbent_rank {
            Some(r) if r == severity_rank(&h) => worst.push((h, name)),
            Some(r) if r > severity_rank(&h) => {}
            _ => worst = vec![(h, name)],
        }' \
  '        let incumbent_rank = worst.first().map(|(w, _)| w.clone());
        match incumbent_rank {
            Some(r) if r == h => worst.push((h, name)),
            Some(r) if r > h => {}
            _ => worst = vec![(h, name)],
        }' \
  geode-data two_orphaned_candidates_with_different_reasons_are_both_named

run_mutation "scheduler: a higher-rank candidate REPLACES the names kept at a lower rank" \
  crates/geode-data/src/ingest/scheduler.rs \
  '            _ => worst = vec![(h, name)],' \
  '            _ => worst.push((h, name)),' \
  geode-data a_higher_rank_candidate_replaces_the_names_accumulated_at_a_lower_rank

run_mutation "runner: the refusal warning is latched once per runner" \
  crates/geode-data/src/ingest/runner.rs \
  'fn log_refused_event(latched: &AtomicBool, what: &str) {
    if latched.swap(true, Ordering::Relaxed) {
        return;
    }' \
  'fn log_refused_event(latched: &AtomicBool, what: &str) {
    if false && latched.swap(true, Ordering::Relaxed) {
        return;
    }' \
  geode-data a_refusal_is_logged_once_per_runner_not_once_per_event

run_mutation "scheduler: the refusal warning is latched once per scheduler" \
  crates/geode-data/src/ingest/scheduler.rs \
  'fn log_refused_discovery(latched: &AtomicBool, what: &str, source: &str) {
    if latched.swap(true, Ordering::Relaxed) {
        return;
    }' \
  'fn log_refused_discovery(latched: &AtomicBool, what: &str, source: &str) {
    if false && latched.swap(true, Ordering::Relaxed) {
        return;
    }' \
  geode-data a_refusal_is_logged_once_per_scheduler_not_once_per_poll

run_mutation "pool: the refusal warning is latched once per worker" \
  crates/geode-data/src/query/pool.rs \
  'fn log_refused_result(latched: &AtomicBool, view: &str) {
    if latched.swap(true, Ordering::Relaxed) {
        return;
    }' \
  'fn log_refused_result(latched: &AtomicBool, view: &str) {
    if false && latched.swap(true, Ordering::Relaxed) {
        return;
    }' \
  geode-data a_refusal_is_logged_once_per_worker_not_once_per_result

# Phase 4c part 2a, Task 1: the modal vocabulary had a forward step
# (`space`) and no way back. Restoring forward-only by dropping the
# `shift+space` arm is invisible to a green suite unless a test asserts
# what `shift+space` maps to.
run_mutation "dialogmode: shift+space maps to nothing (forward-only restored)" \
  crates/geode-shell/src/dialogmode.rs \
  '            "space" => Some(NormalCommand::ToggleBack),' \
  '' \
  geode-shell shift_space_steps_a_value_backward

# `Number` used to refuse at `max` with no way down at all — the sketch
# this guarded against was Groupings' `slot`, which Part 2a Task 4 ruled
# out (display-only, never a `Number` — see `groupings.rs`'s own module
# doc), so this now guards a field no domain has built yet. Making the
# backward arm always refuse (`return false`) is the old defect, and only
# a test asserting the value actually moves down — not just that the row
# is steppable — can see it.
run_mutation "objectdialog: Number refuses to step down" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '                        StepDirection::Backward => *value = (*value - 1).clamp(*min, *max),' \
  '                        StepDirection::Backward => return false,' \
  geode-shell a_number_steps_both_ways_and_stops_at_each_end

# ---- Phase 4c part 2a, Task 4: the Groupings adapter -------------------
#
# The object's own value is a bare array, not a table (`3 = ["book",
# "lhu"]`) — the first domain the write pipeline had to generalise
# `Table` to `Item` for. `presentation_doc` is the other generalisation:
# Groupings has no presentation destination at all, so `Domain::objects`
# and `render::run_confirmed`'s removal-doc list both had to stop
# assuming every domain has one.

run_mutation "groupings: the summary drops the malformed/empty branches" \
  crates/geode-shell/src/shell/objectdialog/groupings.rs \
  '    if names.is_empty() {
        "empty".to_string()
    } else {
        GroupingSlots::label_of(&names)
    }' \
  '    GroupingSlots::label_of(&names)' \
  geode-shell a_malformed_slot_still_describes_itself

run_mutation "groupings: fields duplicates a chain column instead of deduping against the catalogue" \
  crates/geode-shell/src/shell/objectdialog/groupings.rs \
  '        if current.contains(&column.column) {
            continue;
        }' \
  '        if false {
            continue;
        }' \
  geode-shell fields_offers_every_pickable_column_chain_first_then_the_rest

run_mutation "groupings: to_table writes every list item, ticked or not" \
  crates/geode-shell/src/shell/objectdialog/groupings.rs \
  '        if item.included {
            array.push(item.name.as_str());
        }' \
  '        array.push(item.name.as_str());' \
  geode-shell to_table_excludes_unticked_items

run_mutation "groupings: validate discards the reader's own diagnostics" \
  crates/geode-shell/src/shell/objectdialog/groupings.rs \
  '    let (_slots, diags) = GroupingSlots::from_doc(&doc, &schema, &dims);
    diags' \
  '    let (_slots, _diags) = GroupingSlots::from_doc(&doc, &schema, &dims);
    Vec::new()' \
  geode-shell an_out_of_range_slot_key_still_warns_through_validate

# `item_is_empty`'s array branch widened the empty test beyond
# `Table::is_empty` for Groupings' bare-array object shape. Losing it
# makes an empty array a real value again — written rather than recognised
# as empty — and no Views fixture can see that, because Views' own object
# is always a table.
run_mutation "objectdialog: an emptied array is written instead of removed" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '        toml_edit::Item::Value(v) => v.as_array().is_some_and(|a| a.is_empty()),' \
  '        toml_edit::Item::Value(_) => false,' \
  geode-shell an_empty_presentation_rendering_is_an_absence

# ---- whole-branch review fixes (2026-09-10) ----------------------------
#
# MAJ-1 and its three smaller siblings. The Major was a silent divergence
# between what the Groupings edit stage painted and what the app grouped
# by: "empty" collapsed to "remove the user's key", which in a domain's
# own doc means INHERIT THE LAYER BENEATH — so unticking a slot's last
# dimension restored the desk's chain while the stage kept painting an
# empty one. The fix has two halves and each needs its own entry, because
# the refusal makes the semantics unreachable by keystroke and the
# semantics is what a future `Destination::Doc` adapter inherits.

# Half one, the semantics: `Destination::Doc` must never turn an empty
# rendering into an absence. Only a unit test can see this now that the
# keystroke is refused — and it is the exact MAJ-1 defect, restored by
# one word.
run_mutation "objectdialog: an emptied Doc object removes the user's key instead of writing nothing" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '            Destination::Doc => ObjectWrite::Nothing,' \
  '            Destination::Doc => ObjectWrite::Remove,' \
  geode-shell an_empty_doc_rendering_writes_nothing_rather_than_removing_the_key

# Half two, the refusal: a state the config model cannot hold
# (`GroupingSlots::set` refuses an empty chain, `from_doc` warns "slot N
# is empty; ignored") must not be reachable by keystroke. Removing the
# guard lets the untick through, and only a test that compares the PAINTED
# chain against `Frame::active_grouping()` after `ctrl+3` can see the
# divergence that follows — a notice- or file-only assertion cannot.
run_mutation "objectdialog: unticking a Doc list's last entry is allowed again" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '                if included
                    && dest == Destination::Doc
                    && items.iter().filter(|i| i.included).count() == 1
                {' \
  '                if false {' \
  geode-shell unticking_a_slots_last_dimension_leaves_the_painted_chain_and_the_frame_agreeing

# MIN-1: `commit_edit` resolves the user directory BEFORE it moves the
# draft's baseline. Marking the draft saved on the way out of a shell with
# nowhere to write makes an unqueued value the baseline, and a later
# declined `Confirm::Fork` then reverts onto a value that was never
# applied and never persisted. Needs `user_dir: None`, which only one
# fixture in the suite has.
#
# Re-anchored on the preceding "Before the baseline moves" comment (Task
# 3): `commit_create` resolves the SAME `user_dir` with the SAME "nothing
# was changed" wording, so the bare guard-clause anchor stopped being
# unique the moment that sibling function existed.
run_mutation "objectdialog: a shell with nowhere to write still marks the draft saved" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '    // **Before the baseline moves.** A shell with nowhere to write queues
    // nothing, so nothing has been accounted for and the draft must stay
    // dirty: a `mark_saved()` here would make the unqueued value the
    // baseline, and a later declined `Confirm::Fork` would then
    // `revert_to_baseline` onto a forked value that was never applied and
    // never persisted — exactly what `cancel_confirm` exists to prevent.
    let Some(user_dir) = shell.user_dir.clone() else {
        return Some("no writable user config directory — nothing was changed".to_string());
    };' \
  '    // **Before the baseline moves.** A shell with nowhere to write queues
    // nothing, so nothing has been accounted for and the draft must stay
    // dirty: a `mark_saved()` here would make the unqueued value the
    // baseline, and a later declined `Confirm::Fork` would then
    // `revert_to_baseline` onto a forked value that was never applied and
    // never persisted — exactly what `cancel_confirm` exists to prevent.
    let user_dir = match shell.user_dir.clone() {
        Some(dir) => dir,
        None => {
            if let Some(draft) = shell
                .object_dialog
                .as_mut()
                .and_then(|state| state.draft.as_mut())
            {
                draft.mark_saved();
            }
            return Some("no writable user config directory — nothing was changed".to_string());
        }
    };' \
  geode-shell an_edit_with_nowhere_to_write_leaves_the_draft_dirty

# MIN-6: `commit_edit` answers `None` both for "queued" and for "nothing
# changed", so a confirmed `o` over a scope that already equals the
# frame's — the ordinary state after `:scope load` — produced no write and
# no message at all. Every other `o` test stays green: they all overwrite
# a scope that really differs.
run_mutation "objectdialog: a confirmed o that changes nothing says nothing" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '            revalidate(shell);
            if !changed {' \
  '            revalidate(shell);
            if false {' \
  geode-shell o_on_a_scope_that_already_matches_the_frame_says_so

# The review's one missing entry, smaller than MAJ-1: `commit_removal`
# flushes at `Duration::ZERO` rather than behind `WRITE_DEBOUNCE`, because
# a confirmed `d`/`r` is a single act with nothing to coalesce. The
# covering test advances no clock at all, so the debounce would leave the
# deleted row in the browse list.
#
# Re-anchored on `commit_removal`'s own "nothing was removed" guard (Task
# 3): `commit_create`'s `queue_batch(.. Duration::ZERO ..)` call reads
# identically, and the bare-line anchor stopped being unique the moment
# that sibling function existed.
run_mutation "objectdialog: a removal waits on the write debounce" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '    let Some(user_dir) = shell.user_dir.clone() else {
        return Some("no writable user config directory — nothing was removed".to_string());
    };
    queue_batch(shell, edits, user_dir, Duration::ZERO, cx);' \
  '    let Some(user_dir) = shell.user_dir.clone() else {
        return Some("no writable user config directory — nothing was removed".to_string());
    };
    queue_batch(shell, edits, user_dir, WRITE_DEBOUNCE, cx);' \
  geode-shell deleting_a_user_layer_object_leaves_the_browse_list_before_the_watcher_could_fire

# `Domain::objects` used to ask `Destination::Presentation.doc(self)`
# unconditionally, which panics for Groupings (no Presentation-destined
# field to name a doc for). Hardcoding `None` here does not panic — it
# silently drops the personalisation check for every domain, Views
# included, so an existing Views test is what catches it, proving the
# generalisation did not cost Views its own feature.
run_mutation "objectdialog: objects() stops asking any domain for a presentation doc" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '            self.presentation_doc(),' \
  '            None,' \
  geode-shell a_presentation_only_override_is_marked_overridden

# The removal path's own twin: reverting to the old unconditional
# two-file array panics on `Destination::Presentation.doc(Groupings)`
# the moment a trader deletes or reverts a slot — unreachable from any
# Views fixture, since Views is the one domain that DOES have a
# presentation doc.
run_mutation "objectdialog: d/r ask for a presentation doc even when the domain has none" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '            let mut docs = vec![Destination::Doc.doc(domain)];
            if let Some(presentation) = domain.presentation_doc() {
                docs.push(presentation);
            }' \
  '            let docs = [Destination::Doc.doc(domain), Destination::Presentation.doc(domain)];' \
  geode-shell deleting_a_forked_slot_does_not_look_for_a_presentation_doc_that_does_not_exist

# ---- Phase 4c part 2a, Task 5: the Scopes adapter ----------------------
#
# The thinnest adapter (both its fields are read-only `Text`, painted
# from `draft.source` and never fed back into `to_table`), and its one
# new verb: `o` overwrites the saved scope under the cursor with the
# frame's current one. Two properties nothing else in this suite can
# see: that `o` asks before acting (the same "confirm, not immediate"
# shape `d`/`r`/the fork question all share, but this is the one test
# that exercises *this* verb's own arm/confirm wiring), and that the
# write lands in `scopes.toml` rather than in the frame it reads from.

run_mutation "objectdialog: o overwrites immediately instead of asking first" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  "        NormalCommand::Verb('o') => arm_overwrite(shell)," \
  "        NormalCommand::Verb('o') => {
            arm_overwrite(shell);
            let armed = shell
                .object_dialog
                .as_ref()
                .and_then(|state| state.draft.as_ref())
                .and_then(|draft| draft.confirm);
            if let Some(confirm) = armed {
                disarm_confirm(shell);
                run_confirmed(shell, confirm, window, cx);
            }
        }" \
  geode-shell o_confirms_before_overwriting

run_mutation "objectdialog: o writes the frame instead of the saved scope" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '            let changed = draft_mut(shell).is_some_and(|draft| {
                scopes::overwrite_with(draft, &scope);
                draft.is_dirty()
            });' \
  '            let changed = true;
            shell.frame.update(cx, |f, _| {
                f.set_scope(scope.clone());
            });' \
  geode-shell o_overwrites_the_saved_scope_with_the_frames_current_one

# ---- Phase 4c part 2a, Task 5 review round 1: fork disclosure + -------
# ---- source-based dirtiness --------------------------------------------
#
# The Major: `o` used to fork a desk-owned scope into the user layer with
# no disclosure at all — `arm_overwrite` decides `forks` from whether the
# user layer already owns the object, independently of the prompt it
# feeds, so this is the one line that decision collapses to.
#
# The Minor: `is_dirty`/`writes_by_destination` used to compare only the
# painted `Selects`/`Text filter` summaries, which are lossy on purpose
# (`selects_summary` joins values with ", "). A single value containing
# ", " paints identically to several separate values, so a fields-only
# comparison would call that pair "no change" and `o` would silently
# refuse to write. Comparing `source` (the actual object) closes it.

run_mutation "objectdialog: o discloses no fork for a desk-owned scope" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    let forks = editing_row(shell).is_some_and(|row| row.layer != Some(Layer::User));' \
  '    let forks = false;' \
  geode-shell o_on_a_desk_owned_scope_discloses_the_fork_before_writing

run_mutation "objectdialog: is_dirty ignores a source-only change" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '    pub fn is_dirty(&self) -> bool {
        self.fields != self.baseline || self.source != self.baseline_source
    }' \
  '    pub fn is_dirty(&self) -> bool {
        self.fields != self.baseline
    }' \
  geode-shell overwrite_with_is_seen_even_when_the_painted_summary_collides

run_mutation "objectdialog: writes_by_destination ignores a source-only change" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        if self.source != self.baseline_source {
            out.entry(Destination::Doc).or_default();
        }' \
  '        if false {
            out.entry(Destination::Doc).or_default();
        }' \
  geode-shell overwrite_with_is_seen_even_when_the_painted_summary_collides

# §18.4: the roster is what puts an unfilled slot on the list. Dropping it
# leaves every configured-slot test green and the empty rows gone.
run_mutation "objectdialog: groupings roster lists the nine slots" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '            Domain::Groupings => Some(&["1", "2", "3", "4", "5", "6", "7", "8", "9"]),' \
  '            Domain::Groupings => None,' \
  geode-shell \
  groupings_always_lists_nine_slots_and_an_unconfigured_one_has_no_layer

# An unconfigured slot has no layer; forking it would ask a confirm over
# nothing. `is_some_and(.. != User)` says None forks nothing.
run_mutation "objectdialog: an unconfigured slot never forks" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '        .is_some_and(|row| row.layer.is_some_and(|layer| layer != Layer::User))' \
  '        .is_some_and(|row| row.layer != Some(Layer::User))' \
  geode-shell \
  ticking_a_dimension_in_an_empty_slot_writes_it_without_asking

# A new view must start on a real dataset rather than an empty
# placeholder: the trader would otherwise have to notice the empty
# `Dataset` row and step it before the view selected anything at all.
# (Not a write gate: `ViewSpec::from_doc` rates a missing dataset
# `Severity::Warning`, which `apply::blocking_diagnostic` deliberately
# lets through — only an error blocks. The mutation is caught by the
# named pure test's own `choice("dataset")` assertion.)
run_mutation "objectdialog: a new view starts on the first real dataset" \
  crates/geode-shell/src/shell/objectdialog/views.rs \
  '        None => options.first().cloned().unwrap_or_default(),' \
  '        None => String::new(),' \
  geode-shell \
  a_new_view_draft_picks_the_first_real_dataset_and_no_columns_of_its_own

# Create flushes at zero debounce like a removal, not 250 ms like an edit.
# The anchor is longer than just the `queue_batch` tail: `commit_removal`
# ends in the exact same three lines (`queue_batch(..Duration::ZERO..);
# None }`), so only including `commit_create`'s own `mark_saved()` guard
# above it keeps the anchor at one match rather than two.
run_mutation "objectdialog: create does not wait for the edit debounce" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '    if let Some(draft) = shell.object_dialog.as_mut().and_then(|s| s.draft.as_mut()) {
        draft.mark_saved();
    }
    queue_batch(shell, edits, user_dir, Duration::ZERO, cx);
    None
}' \
  '    if let Some(draft) = shell.object_dialog.as_mut().and_then(|s| s.draft.as_mut()) {
        draft.mark_saved();
    }
    queue_batch(shell, edits, user_dir, WRITE_DEBOUNCE, cx);
    None
}' \
  geode-shell \
  commit_create_writes_one_doc_entry_immediately

# ---- Phase 4c part 2 refinement, Task 4: Views' two-section column ----
# ---- list — members and available --------------------------------------

# Membership is definitional: taking the catalogue's names in too would
# call an add "no membership change" — every add promotes a name already
# painted — and route it to the overlay, silently never writing views.toml.
# §18.7 re-anchored this from the old `member` filter onto the arm that now
# carries the behaviour: which LIST is read.
run_mutation "objectdialog: membership compares the object's own names only" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        FieldKind::OrderedList { items, .. } => Some(
            items
                .iter()
                .map(|i| i.name.clone())
                .collect::<BTreeSet<_>>(),
        ),' \
  '        FieldKind::OrderedList { items, available } => Some(
            items
                .iter()
                .chain(available.iter().flatten())
                .map(|i| i.name.clone())
                .collect::<BTreeSet<_>>(),
        ),' \
  geode-shell \
  adding_an_available_column_is_a_doc_write_and_hiding_is_not

# An added column joins the END of the object's own list — the order of
# that list is the view's own column order, and a column arriving at its
# front is a reorder the trader never asked for. Covered by promoting a
# LATER catalogue row, not the first one: for the catalogue's first row,
# its own index and the end of a one-column list are numerically the same,
# so that case cannot tell the two apart.
run_mutation "objectdialog: an added item joins the end of the object's own list" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '                items.push(entry);' \
  '                items.insert(0, entry);' \
  geode-shell \
  adding_a_later_available_column_still_joins_the_end_of_the_views_own_list

# User ruling 2026-09-11: after `space` adds a column the cursor moves on
# to the NEXT available row (the old visible index plus one — the added
# item moved earlier in row order, so the rows ahead of the next one are
# the same set), not with the added item into the member block. Dropping
# the `+ 1` leaves it on whatever row now holds the old index — the
# added item's former neighbour above — which is the wrong row.
run_mutation "objectdialog: an add leaves the cursor on the row above the next one" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '                self.selected = (self.selected + 1).min(last);' \
  '                self.selected = self.selected.min(last);' \
  geode-shell \
  adding_a_column_leaves_the_cursor_on_the_next_available_row

# And the clamp: adding the block's last row has no next row, so without
# it the cursor indexes one past the visible list and `selected_row`
# answers `None` — every verb goes inert until the trader presses `k`.
run_mutation "objectdialog: an add of the last row runs the cursor off the end" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '                self.selected = (self.selected + 1).min(last);' \
  '                self.selected = self.selected + 1;' \
  geode-shell \
  adding_the_last_available_column_leaves_the_cursor_on_the_row_before

# RETIRED by §18.7 (2026-09-12): "objectdialog: reorder never crosses the
# member boundary". The boundary was a rule the ordering of ONE list had
# to maintain, and the entry mutated the comparison that enforced it.
# There is no boundary now — `move_item` walks `items`, and the catalogue
# is a different vector it cannot reach — so the behaviour the entry
# guarded is not representable and an entry over it would have nothing to
# break. What CAN still go wrong is the other side of the same rule: an
# available row's index read as a position in the object's own list. That
# is the entry below, on the arm that declines it.
#
# The mutation hands `move_item` an available row's index as if it were an
# item's. The named test's fixture is what makes that visible: two columns
# of the view's own, one catalogue row at index 0 — so the misread index
# names `npv`, which really can move, and the reorder happens instead of
# being declined. With a one-column view the same misread would run off
# the end and answer `None` for the wrong reason, and the entry would read
# "caught" while defending nothing.
run_mutation "objectdialog: an available row is not reorderable" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '            EditRow::Available { .. } | EditRow::Field(_) => return None,' \
  '            EditRow::Available { field, item } => (field, item),
            EditRow::Field(_) => return None,' \
  geode-shell \
  an_available_row_cannot_be_reordered

# ---- Task 4 review round 1: kind, dest-based membership, dataset ------
# ---- switch (2026-09-10) ------------------------------------------------
#
# Four Important findings: an added dimension (or a schema role with no
# honest kind) was written kind-less and silently mis-resolved as a
# measure; the Doc-table member filter — the one line keeping an
# available column out of views.toml — had no covering test at all;
# `remove_selected` decided "does this list have separate membership" by
# scanning items rather than by the field's own `dest`, so `x` went dead
# the moment a trader finished adding every available column; and
# changing the dataset left the available block showing the OLD
# dataset's columns. Derived dimensions came out of the available block
# entirely rather than being fixed forward — `views::fields`'s own doc
# has the full reasoning (no `[[columns]]` `kind` makes one queryable).

# A missing `kind` defaults to `"measure"` in `ViewSpec::from_doc`, so an
# added dimension resolved as a measure compiles to nothing (or, worse,
# a real measure of the same name) — silently, and only visible once the
# blotter comes back wrong.
run_mutation "objectdialog: a newly written column carries its real kind" \
  crates/geode-shell/src/shell/objectdialog/views.rs \
  '            column["kind"] = toml_edit::value(kind.as_str());' \
  '            let _ = kind;' \
  geode-shell \
  adding_an_available_column_is_a_doc_write_and_hiding_is_not

# The single line keeping an available column out of `views.toml` — an
# available row promoted to nothing would still fork the desk's view AND
# list a column the trader never asked to add.
run_mutation "objectdialog: the doc table lists the view's own columns only" \
  crates/geode-shell/src/shell/objectdialog/views.rs \
  '        let wanted: Vec<&ListItem> = items.iter().collect();' \
  '        let wanted: Vec<&ListItem> = items
            .iter()
            .chain(draft.available_items("columns").unwrap_or_default())
            .collect();' \
  geode-shell \
  adding_an_available_column_is_a_doc_write_and_hiding_is_not

# Whether a list has a separate membership is whether it HAS a catalogue
# (§18.7.2): Groupings' `dimensions` has none — ticking already IS
# membership there — so `x` must refuse and name `space`. Losing the
# refusal makes `x` silently untick a dimension instead.
run_mutation "objectdialog: x removes from a list that has no catalogue at all" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        let Some(available) = available.as_mut() else {
            return Step::Refused("space unticks here".to_string());
        };' \
  '        let available = available.get_or_insert_with(Vec::new);' \
  geode-shell \
  a_groupings_list_has_no_available_block_and_x_refuses

# And the other half of the same line, which is the regression the
# `Option` exists for: an EMPTY catalogue is not the absence of one. Read
# as "no catalogue", `x` goes dead on a Views list the moment the trader
# adds the last column the dataset offers — the exact failure the
# `member`-scanning build had, reachable again through emptiness.
run_mutation "objectdialog: an empty catalogue reads as no catalogue" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        let Some(available) = available.as_mut() else {
            return Step::Refused("space unticks here".to_string());
        };' \
  '        let Some(available) = available.as_mut().filter(|a| !a.is_empty()) else {
            return Step::Refused("space unticks here".to_string());
        };' \
  geode-shell \
  remove_selected_still_works_when_the_catalogue_is_empty

# `x` on an available row must refuse — there is nothing there yet to
# remove — and name the verb that actually adds it, not fall through to
# `items.remove`/`items.push` and quietly reorder the row instead.
run_mutation "objectdialog: x refuses an available row rather than removing by its index" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '            Some(EditRow::Available { .. }) => {
                return Step::Refused("not in the view — space adds it".to_string());
            }' \
  '            Some(EditRow::Available { field, item }) => (field, item),' \
  geode-shell \
  x_on_an_available_row_says_not_in_the_view

# Changing the dataset must empty and repopulate the available block, or
# a trader who switches datasets keeps ticking columns the new dataset
# does not even have.
run_mutation "objectdialog: refresh_available empties the old dataset's rows" \
  crates/geode-shell/src/shell/objectdialog/views.rs \
  '    *available = Some(rebuilt);' \
  '    available.get_or_insert_with(Vec::new).extend(rebuilt);' \
  geode-shell \
  refresh_available_repopulates_for_the_newly_chosen_dataset

# ---- the drag grab's focus trap (2026-09-09) ---------------------------
#
# Two halves of the same defect: a tile mouse-down that arms a drag
# returns before the caller's click-to-focus tail, so it has to re-arm
# `pending_focus_restore` itself, and `render` carries a safety net for
# the no-focus state generally. Both failure modes are silent — the app
# paints perfectly and simply stops answering the keyboard.

run_mutation "focus: the drag grab re-arms the focus restore" \
  crates/geode-shell/src/shell/drag.rs \
  '        self.pending_focus_restore = true;
        cx.stop_propagation();' \
  '        cx.stop_propagation();' \
  geode-shell a_grab_leaves_the_shell_focused_on_the_next_frame

run_mutation "focus: a window with nothing focused gets the shell root back" \
  crates/geode-shell/src/shell/render.rs \
  '        if window.focused(cx).is_none() {
            self.focus_handle.focus(window, cx);
        }' \
  '        if false {
            self.focus_handle.focus(window, cx);
        }' \
  geode-shell a_window_with_nothing_focused_gets_the_shell_root_back_on_the_next_frame

# The other direction on the same line: the net's condition is exactly
# `is_none()`, and the tempting broader form ("focus isn't the shell
# root") pulls the caret out of every live focused `Input` on every
# frame. Without a test that keeps one focused across a redraw, nothing
# would notice.
run_mutation "focus: the no-focus net never steals from a live focused element" \
  crates/geode-shell/src/shell/render.rs \
  '        if window.focused(cx).is_none() {
            self.focus_handle.focus(window, cx);
        }' \
  '        if !self.focus_handle.is_focused(window) {
            self.focus_handle.focus(window, cx);
        }' \
  geode-shell the_focus_net_leaves_a_live_focused_input_alone

# The `is_none()` net above cannot see a workspace switch (occupants are
# retained across workspaces, so focus stays `Some`); `ensure_occupants`
# carries the backstop that can. Same two directions: it must reclaim,
# and it must not reclaim from a live shell surface.
run_mutation "focus: a departed tile's focus returns to the shell root" \
  crates/geode-shell/src/shell/occupants.rs \
  '        if any_tile_left_the_screen
            && let Some(focused) = window.focused(cx)
            && !self.holds_shell_focus(&focused, cx)
        {
            self.focus_handle.focus(window, cx);
        }' \
  '        if any_tile_left_the_screen
            && let Some(focused) = window.focused(cx)
            && !self.holds_shell_focus(&focused, cx)
        {
            let _ = &focused;
        }' \
  geode-shell a_focused_tile_leaving_the_visible_set_hands_focus_back_to_the_shell

run_mutation "focus: the departed-tile backstop spares the shell's own surfaces" \
  crates/geode-shell/src/shell/occupants.rs \
  '            && !self.holds_shell_focus(&focused, cx)' \
  '            && !false' \
  geode-shell a_tile_leaving_the_visible_set_leaves_the_palette_focused

# ---- startup config diagnostics reach the entity (2026-09-09) ---------
#
# The mutation is the old code: seed the entity from
# `config.diagnostics` alone. Neither the refused `keymap.mod` alias nor
# the retired `[app] modules.default` key lives in that list, so the
# diagnostics tile silently omitted both until a hot reload happened to
# add them — a diagnostic that exists, is logged, and is invisible where
# a trader would look for it.

# The anchor deliberately ENDS on the keymap extend rather than on the
# block's bare `diags` tail (fix round 2). Matching is exact-substring,
# so a `from` ending in `\n            diags` matches the PREFIX of the
# next line, `diags.extend(services.keymap_diagnostics…)` — the mutated
# body then read `services.config.diagnostics.clone().extend(…); diags`
# with `diags` unbound, and a compile error is reported as a plain
# `caught` with the named test never run. The header's "an entry can lie"
# case, and the reason an anchor must end somewhere no live line begins.
run_mutation "diagnostics: startup seeding folds in the computed config diagnostics" \
  crates/geode-shell/src/shell/mod.rs \
  '            let mut diags = cfg.diagnostics.clone();
            diags.extend(crate::defaults::mod_alias_from_config(cfg).1);
            diags.extend(crate::defaults::modules_default_diagnostic(cfg));
            diags.extend(services.keymap_diagnostics.iter().cloned());' \
  '            let mut diags = cfg.diagnostics.clone();
            diags.extend(services.keymap_diagnostics.iter().cloned());' \
  geode-shell a_modules_default_key_is_in_the_diagnostics_entity_at_startup

# The fourth group is the one that cannot be recomputed — it rides on
# `ShellServices::keymap_diagnostics` — so dropping the extend is silent
# in a way the other three are not: nothing else would ever put a
# `build_keymap` diagnostic in front of a trader at startup.
run_mutation "diagnostics: the startup seeding carries build_keymap's own diagnostics" \
  crates/geode-shell/src/shell/mod.rs \
  '            diags.extend(services.keymap_diagnostics.iter().cloned());' \
  '' \
  geode-shell startup_keymap_diagnostics_are_in_the_diagnostics_entity

# ---- `n`: the naming row, create, and the edit stage on a new object
# (§18.2, Part 2 Task 5) ------------------------------------------------

# A name a layer already holds must be refused: creating `tree` would
# fork the desk's view under a verb that never said so.
run_mutation "objectdialog: n refuses an existing name" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    if domain.name_taken(&shell.services.config, &name) {' \
  '    if false {' \
  geode-shell \
  n_refuses_a_name_any_layer_already_holds

# The third of `name_taken`'s union, and the only one with no row to show
# for it: a name held by the user's `view_presentation.toml` alone (the
# desk dropped a view the trader had hidden a column on). Dropping this
# clause puts the row-based check back, and `n` on that name creates a
# user view that silently inherits the orphaned overlay — with `r`
# refusing it afterwards, since the new view is user-only.
run_mutation "objectdialog: a create ignores a name the presentation overlay holds" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        personalised_names(config, self.presentation_doc()).contains(name)' \
  '        false' \
  geode-shell \
  a_presentation_only_name_is_taken_even_with_no_row_to_show_for_it

# Scopes' n saves the FRAME's scope, not the empty object.
run_mutation "objectdialog: n on scopes reads the frame" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '        scopes::overwrite_with(&mut draft, &scope);' \
  '        let _ = &scope;' \
  geode-shell \
  n_on_a_scope_saves_the_frames_current_scope

# Review round 1: `begin_naming` clears only `state.query`; a stale
# browse filter left in the shared `Input` (typed, then `escape`'d back
# to normal mode, which keeps the query applied) must still be emptied
# before the name field takes focus, or the field opens pre-filled and a
# few keystrokes on top of it can silently fork the wrong desk object.
# Re-anchored onto the pure clear when §16.1 made the transition pure:
# the render-side `set_value`/focus pair this used to mutate is gone, and
# `dialog::sync_dialog_text` now writes the field from
# `effective_query` — so dropping the clear writes the stale browse
# filter straight back into the name field, which is the same defect
# through the new mechanism. Anchored on the two preceding lines as well
# because `self.query.clear();` alone appears in four other transitions
# in this file.
run_mutation "objectdialog: n empties a leftover browse filter before naming" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '    pub fn begin_naming(&mut self) {
        self.stage = Stage::Naming;
        self.query.clear();' \
  '    pub fn begin_naming(&mut self) {
        self.stage = Stage::Naming;' \
  geode-shell \
  n_opens_an_empty_name_field_even_after_a_browse_filter

# §16.1: the confirm buttons are the only door into `run_confirmed` that
# never passes through `ShellView::handle_key_down`, so the sync at the
# end of the "yes" closure is the one that empties the shared field when
# a confirmed delete walks back to browse. Reachable only with the
# mouse: an armed confirm swallows every keystroke, `/` included, so the
# keyboard cannot reach one from filter mode at all — but the action
# bar's buttons are live in either mode. Anchored on the last comment
# line too: the identical sync call appears four times in this file, and
# the "no" closure's is indented identically.
run_mutation "objectdialog: a mouse-confirmed delete leaves its filter text in the field" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '                                // was filtering with is emptied here or nowhere.
                                dialog::sync_dialog_text(shell, window, cx);' \
  '                                // was filtering with is emptied here or nowhere.
                                let _ = window;' \
  geode-shell \
  confirming_with_the_mouse_while_filtering_empties_the_field

# ---- Task 6: filtering the edit stage (§18.3) --------------------------

# The cursor indexes the FILTERED list. Reading rows()[selected] instead
# acts on whatever row happens to sit at that unfiltered index.
run_mutation "objectdialog: the edit cursor indexes the filtered rows" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        self.visible_rows()
            .get(self.selected)
            .and_then(|m| rows.get(m.row).copied())' \
  '        rows.get(self.selected).copied()' \
  geode-shell \
  the_edit_stage_filters_by_label_and_the_cursor_indexes_the_filtered_list

# A reorder under a filter must land past the hidden neighbours, not swap
# with one of them (which looks like nothing happened).
run_mutation "objectdialog: reorder skips hidden rows" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '            if row_of(target).is_some_and(|r| visible.contains(&r)) {
                break;
            }' \
  '            break;' \
  geode-shell \
  reordering_under_a_filter_moves_past_the_hidden_rows_and_says_how_many

# set_query has to reach the draft while the edit stage is open, or
# typing narrows the browse copy and the edit stage keeps painting every
# row.
#
# Review round 1, finding 1: an earlier build of this branch wrote BOTH
# `draft.query` and `self.query` unconditionally, which leaked the edit
# stage's own filter into the browse query it steps back onto (a stale
# `state.query` the edit stage's own `ClearQuery` rung never touches).
# The mirror is now gated on `stage` — anchored on that gate itself
# rather than on the assignment inside it, since the assignment alone
# (`draft.query = query`) looks identical to the buggy version and would
# not tell the two apart.
run_mutation "objectdialog: set_query mirrors into the open draft only in the edit stage" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        if matches!(self.stage, Stage::Edit { .. })
            && let Some(draft) = self.draft.as_mut()' \
  '        if false
            && let Some(draft) = self.draft.as_mut()' \
  geode-shell \
  slash_filters_the_edit_stage_and_escape_walks_the_full_ladder

# Review round 1, finding 2: the edit stage's row order IS the data
# (column order, chain order), so `rank`'s fuzzy-score ordering must be
# undone afterwards — a mutation that drops the `sort_by_key` leaves
# `visible_rows` back in score order, exactly the defect this finding
# reported (a later, better-scoring match painted ahead of an earlier,
# worse-scoring one).
run_mutation "objectdialog: the edit stage's visible rows stay in row order" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        let mut ranked = crate::listfilter::rank(&labels, &self.query);
        ranked.sort_by_key(|m| m.row);
        ranked' \
  '        crate::listfilter::rank(&labels, &self.query)' \
  geode-shell \
  visible_rows_lists_matches_in_row_order_not_score_order

# ---- Task 7: shared chrome — the modal title slot (§18.1) --------------

# The placeholder is what tells a trader in normal mode that `/` exists.
run_mutation "dialog: the frozen empty filter shows its placeholder" \
  crates/geode-shell/src/shell/dialog.rs \
  '"press / to filter"' \
  '""' \
  geode-shell \
  the_keybinding_dialogs_pill_sits_in_the_title_row

# The title row's own slot: drop what a dialog built for it and every
# modal silently loses its pill (and, from Task 8, its crumb) while the
# title, the close button and the whole dialog below still paint — the
# "a marker is missing, not wrong" failure a bounds-only test on the
# dialog's content cannot see.
run_mutation "dialog: the modal title row paints its title_extra" \
  crates/geode-shell/src/shell/dialog.rs \
  'children(title_extra)' \
  'children(None::<AnyElement>)' \
  geode-shell \
  the_object_dialogs_pill_sits_in_the_title_row_in_both_stages

# Drop the `slash_filters` half of the placeholder's condition and the
# hint comes back during a keybinding capture, where `/` is the binding
# being recorded — a hint for a key that does something else, beside a
# footer already saying "Listening". The empty-query half alone is not a
# defence: every other frozen caller passes `true`, so the whole suite
# stays green on the mutation but for the one dialog that can say `false`.
run_mutation "dialog: the filter placeholder needs / to actually filter" \
  crates/geode-shell/src/shell/dialog.rs \
  '&& frozen.slash_filters' \
  '&& true' \
  geode-shell \
  the_filter_placeholder_is_gone_while_listening_for_a_capture

# ---- Mouse parity (spec §17.1 rule 1): the frozen filter row -----------

# §17.1 rule 1: the frozen filter row is the mouse form of `/`. Mutated
# to a click that syncs without the transition, the row is inert and the
# dialog stays in normal mode under the click.
run_mutation "dialog: a click on the frozen filter row enters filter mode" \
  crates/geode-shell/src/shell/dialog.rs \
  '                        enter_filter_by_mouse(shell);
                        sync_dialog_text(shell, window, cx);' \
  '                        sync_dialog_text(shell, window, cx);' \
  geode-shell clicking_the_frozen_filter_row_enters_filter_mode

# The listening half of the same rule: a capture must be cancelled by the
# click, or `focus_target` keeps the keys on the shell root under a pill
# reading `filter`.
run_mutation "dialog: the frozen-row click cancels a capture in progress" \
  crates/geode-shell/src/shell/dialog.rs \
  '        state.listening = None;
        state.mode = DialogMode::Filter;' \
  '        state.mode = DialogMode::Filter;' \
  geode-shell clicking_the_frozen_filter_row_while_listening_cancels_the_capture

# ---- Task 8: the object dialog to the mock (§18.1) ---------------------

# The section header rides on the first item's element so the edit
# list's child count still equals its row count; a header emitted as its
# own `list.child(header)` before the row instead shifts every later
# child one index further than `Draft::selected` expects, so
# `scroll_to_item` scrolls the wrong child to the target position. On a
# fixture long enough to actually need scrolling (§18.1's fixture below,
# `services_with_a_long_desk_view`: 16 columns, two member and two
# section headers before the tail of the available block), `shift-g`
# lands the true last row (`m13`) short of the bottom of the scrolled
# viewport instead of flush with it — verified by hand: this mutation
# panics `the_cursor_stays_in_view_past_a_section_header_on_a_long_list`
# with "the last row (bottom 706px) should be fully inside the list's
# viewport (bottom 658.5px)".
run_mutation "objectdialog: section headers do not add list children" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '        list = list.child(match section_header {
            Some(header) => v_flex().child(header).child(row_el).into_any_element(),
            None => row_el,
        });' \
  '        if let Some(header) = section_header {
            list = list.child(header);
        }
        list = list.child(row_el);' \
  geode-shell \
  the_cursor_stays_in_view_past_a_section_header_on_a_long_list

# The edit list used to size itself by summing a fixed
# `FIELD_ROW_HEIGHT` per visible row, capped at `VISIBLE_ROWS *
# ROW_HEIGHT` — but since §18.1 folded a section header into a block's
# first item, the sum undercounted by one header's height per painted
# block whenever the list was short enough not to hit the cap, and
# clipped the last row. Reverting to that fixed-height form (with
# `FIELD_ROW_HEIGHT` reintroduced inline, since the constant itself is
# gone) reproduces the clipping on a filtered single-row list.
run_mutation "objectdialog: the edit list sizes itself instead of the header it folds in" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '        .max_h(px(VISIBLE_ROWS as f32 * ROW_HEIGHT))' \
  '        .h(px((visible.len().max(1) as f32 * 28.0).min(VISIBLE_ROWS as f32 * ROW_HEIGHT)))' \
  geode-shell \
  a_filtered_single_row_is_not_clipped_by_the_lists_height

# ---- The final whole-branch review's fix wave (§18.6) ------------------

# §18.3 made the edit stage reachable in filter mode, which its click
# handler predates: focusing the shell unconditionally leaves the pill
# reading `filter` and the caret painted over a blurred `Input`, and the
# next keystroke goes nowhere. Since §16.1 the handler asks
# `dialog::sync_dialog_text` where focus belongs instead of branching on
# the mode itself, so the mutation is that call replaced by the old
# unconditional blur — the same defect, one seam later. The anchor
# carries the preceding `scroll_to_item(position)` line because browse's
# own `on_row_clicked` ends in the identical sync call
# (`scroll_to_item(ix)` there), which would otherwise make this anchor
# ambiguous.
run_mutation "objectdialog: an edit-stage click takes the keyboard off the filter" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    shell.object_dialog_scroll.scroll_to_item(position);
    dialog::sync_dialog_text(shell, window, cx);' \
  '    shell.object_dialog_scroll.scroll_to_item(position);
    shell.focus_handle.focus(window, cx);' \
  geode-shell \
  clicking_an_edit_row_while_filtering_keeps_the_filter_focused

# `space` promotes a row to the END of the member block — a screenful
# away on a list that scrolls — so the cursor has to be scrolled back
# into view, exactly as `shift+j`'s own arm does. Anchored from the
# `Toggle` arm's head because `ToggleBack`'s body below is character-for-
# character identical.
run_mutation "objectdialog: space moves the cursor off screen and leaves it there" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '        NormalCommand::Toggle => match draft_mut(shell).map(Draft::toggle_selected) {
            Some(Step::Changed) => {
                maybe_refresh_available(shell);
                revalidate(shell);
                scroll_to_cursor(shell);' \
  '        NormalCommand::Toggle => match draft_mut(shell).map(Draft::toggle_selected) {
            Some(Step::Changed) => {
                maybe_refresh_available(shell);
                revalidate(shell);' \
  geode-shell \
  space_scrolls_the_next_row_into_view

# The same for `shift+space`: one `step_selected` underneath, two arms
# above it, so a fix applied to one and not the other is exactly the
# failure this pair of entries exists to notice.
run_mutation "objectdialog: shift+space moves the cursor off screen and leaves it there" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '        NormalCommand::ToggleBack => match draft_mut(shell).map(Draft::toggle_selected_back) {
            Some(Step::Changed) => {
                maybe_refresh_available(shell);
                revalidate(shell);
                scroll_to_cursor(shell);' \
  '        NormalCommand::ToggleBack => match draft_mut(shell).map(Draft::toggle_selected_back) {
            Some(Step::Changed) => {
                maybe_refresh_available(shell);
                revalidate(shell);' \
  geode-shell \
  shift_space_scrolls_the_next_row_into_view

# And `x`, which moves the cursor the other way — to the end of the
# available block, off the BOTTOM of the viewport.
# `x` no longer scrolls (user ruling 2026-09-11: the cursor stays at its
# own visible index — the row that was next — rather than following the
# demoted item to the end of the available block, so the `x` arm's old
# `scroll_to_cursor` call went with the old behaviour; an entry over a
# call no test could see would be a harness lie). Two entries over the
# rule itself instead. Inverting the "landed back on the removed item"
# check makes the ORDINARY removal step back one row (the previous
# member instead of the next) and the last-row removal stay on the
# removed item.
run_mutation "objectdialog: x steps back on every removal but the last" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        if under_cursor == Some(moved) {' \
  '        if under_cursor != Some(moved) {' \
  geode-shell \
  x_leaves_the_cursor_on_the_next_row

# And the fallback itself: without the step back, removing the list's
# last row leaves the cursor on the row it just removed, so the next
# `space` quietly puts it back.
run_mutation "objectdialog: x on the last row leaves the cursor on the removed item" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '            self.selected = self.selected.saturating_sub(1);
        }
        Step::Changed' \
  '            self.selected = self.selected;
        }
        Step::Changed' \
  geode-shell \
  x_on_the_last_row_steps_the_cursor_back_to_the_previous_row

# A `Key` or `Attribute` column has no honest `[[columns]]` kind, so it
# is not offered in the available block at all. Forcing either into
# "dimension" writes a kind that is not true of the column — the silent
# wrong-data defect `ListItem.kind` exists to remove. One entry over the
# shared arm: the test asserts the two roles separately, so it fails
# whichever half a future split got wrong.
run_mutation "objectdialog: a Key or Attribute column is offered as a dimension" \
  crates/geode-shell/src/shell/objectdialog/views.rs \
  '        ColumnRole::Key | ColumnRole::Attribute { .. } => None,' \
  '        ColumnRole::Key | ColumnRole::Attribute { .. } => Some("dimension"),' \
  geode-shell \
  key_and_attribute_columns_are_not_offered

# ---- Dialog text sync, Task 1: the pure decision and the effective query ----
#
# §16.3: a capture must read raw keystrokes off the shell root. Dropping the
# `listening` override focuses the Input mid-capture and the captured `a`
# becomes text — the keybinding dialog's original modal defect.
run_mutation "dialogmode: listening overrides the mode for focus" \
  crates/geode-shell/src/dialogmode.rs \
  '    if listening {
        return FocusTarget::Shell;
    }' \
  '    if false {
        return FocusTarget::Shell;
    }' \
  geode-shell \
  focus_follows_the_mode_unless_a_capture_is_listening

# §16.2: the read half of the one-way mirror. Reading self.query in the edit
# stage paints (and, after Task 3, WRITES into the Input) the browse query.
run_mutation "objectdialog: effective_query reads the edit stage's draft" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '            (Stage::Edit { .. }, Some(draft)) => draft.query.as_str(),' \
  '            (Stage::Edit { .. }, Some(_draft)) => self.query.as_str(),' \
  geode-shell \
  the_effective_query_is_the_stages_own

# ---- Dialog text sync, Task 2: the reconcile itself ----
#
# §16.1, focus rule: the sync is the only thing that moves focus now — no
# site in `keybindings_view` or `objectdialog::render`'s key path touches a
# focus handle. Focusing the shell root in filter mode leaves every typed
# character falling on the floor, and only a test that asserts on the
# FOCUSED SURFACE between transitions can see it (a query assertion cannot:
# the mirror is still whatever the Input last emitted).
run_mutation "dialog: the sync focuses the Input in filter mode" \
  crates/geode-shell/src/shell/dialog.rs \
  '        FocusTarget::Input => input.read(cx).focus_handle(cx).focus(window, cx),' \
  '        FocusTarget::Input => shell.focus_handle.focus(window, cx),' \
  geode-shell \
  focus_and_text_follow_the_pure_state_through_every_transition

# §16.1, text rule: the Input is written from the effective query. Dropping
# the write leaves a cleared query still painted in the field — the exact
# "every clear had to be written twice, and one cut forgot one" defect the
# amendment exists to remove. Every `state.query` assertion stays green.
run_mutation "dialog: the sync writes the Input from the query" \
  crates/geode-shell/src/shell/dialog.rs \
  '        input.update(cx, |i, cx| i.set_value(query, window, cx));' \
  '        let _ = query;' \
  geode-shell \
  focus_and_text_follow_the_pure_state_through_every_transition

# ---- Groupings: digit jump and the chain field (§18.8, 2026-09-12) ----

# `0` names no slot (`ctrl+0` CLEARS the frame's slot). Widening the range
# makes a bare `0` dispatch a jump to a slot "0" that `GroupingSlots` never
# holds — an empty edit stage over an object the roster does not list.
run_mutation "dialogmode: a bare 0 is a digit command" \
  crates/geode-shell/src/dialogmode.rs \
  "                (Some(c @ '1'..='9'), None) => Some(NormalCommand::Digit(c as u8 - b'0'))," \
  "                (Some(c @ '0'..='9'), None) => Some(NormalCommand::Digit(c as u8 - b'0'))," \
  geode-shell \
  bare_digits_one_to_nine_are_a_command_and_zero_is_not

# The browse jump is gated on the domain: inverting the gate sends a `3`
# typed at the Views list into an edit stage for a view named "3" while
# Groupings drops the very key its footer advertises.
run_mutation "objectdialog: the browse digit jump ignores the domain" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '            NormalCommand::Digit(n) if state.domain == Domain::Groupings => {' \
  '            NormalCommand::Digit(n) if state.domain != Domain::Groupings => {' \
  geode-shell \
  a_digit_in_browse_opens_that_slot_on_groupings_only

# The edit-stage jump has its own gate, spelled differently (an
# `Option<Domain>` read off the state), so it is guarded separately.
run_mutation "objectdialog: the edit-stage digit jump ignores the domain" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '            if domain == Some(Domain::Groupings) {' \
  '            if domain != Some(Domain::Groupings) {' \
  geode-shell \
  a_digit_in_the_edit_stage_jumps_to_that_slot

# Re-entering the open slot rebuilds the draft from a config that can be a
# debounce window behind the last tick — a visible loss of an edit that
# is in fact queued. Dropping the guard keeps every stage assertion green
# (the stage is `Edit { 3 }` either way); only the notice tells.
run_mutation "objectdialog: the open slot's own digit re-enters the stage" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    if already {' \
  '    if false {' \
  geode-shell \
  a_digit_in_the_edit_stage_jumps_to_that_slot

# Whitespace is a separator by user ruling (2026-09-12). Dropping it makes
# `book lhu` one unknown name, refused — every `/`-spelled fixture stays
# green.
run_mutation "groupings: only / separates a chain" \
  crates/geode-shell/src/shell/objectdialog/groupings.rs \
  "    c == '/' || c.is_whitespace()" \
  "    c == '/'" \
  geode-shell \
  parse_chain_accepts_slash_and_space_separators

# The typed order IS the chain order (`GroupingSlots` groups by it).
# Prepending instead of appending reverses it; every fixture that types
# the chain back in its existing order, or types one name, stays green.
run_mutation "groupings: an applied chain is reversed" \
  crates/geode-shell/src/shell/objectdialog/groupings.rs \
  '            item.included = true;
            out.push(item);' \
  '            item.included = true;
            out.insert(0, item);' \
  geode-shell \
  applying_a_chain_ticks_the_typed_names_in_typed_order

# A name already typed is not offered again: dropping the exclusion puts
# `book` back at the top of the completions after `book / `, so `tab`
# completes to a duplicate `enter` then refuses.
run_mutation "groupings: completions offer names already typed" \
  crates/geode-shell/src/shell/objectdialog/groupings.rs \
  '        is_dimension && !done.contains(&labels[m.row])' \
  '        is_dimension' \
  geode-shell \
  chain_candidates_rank_the_trailing_segment_and_skip_completed_names

# A duplicate must be refused, not silently collapsed: without the check
# the second `book` is looked up in a `rest` the first already removed it
# from, and the `expect` below panics.
run_mutation "groupings: a duplicated name is not refused" \
  crates/geode-shell/src/shell/objectdialog/groupings.rs \
  '            .find(|(i, name)| names[..*i].contains(name))' \
  '            .find(|(i, name)| names[..*i].contains(name) && false)' \
  geode-shell \
  applying_refuses_an_unknown_or_duplicated_name_and_stays_open

# The chain handler must run AHEAD of filter mode — the two share a
# focused `Input`, and filter mode's `enter` is a notice, its `tab` a
# drop. Skipping the dispatch leaves the field open with `enter` saying
# "read-only" and `tab` doing nothing; `i` itself still works.
run_mutation "objectdialog: the chain field's keys fall through to filter mode" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    if chain_entry {' \
  '    if false {' \
  geode-shell \
  i_opens_the_chain_field_tab_completes_and_enter_writes_the_chain

# The pill has to say `chain`, not `filter`, over a field whose text is a
# value: `enter` applies it rather than opening a row. The mode really is
# `Filter` underneath, so every mode assertion stays green.
run_mutation "objectdialog: the chain field wears the filter pill" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '                if s.draft.as_ref().is_some_and(|d| d.chain_entry) {' \
  '                if false {' \
  geode-shell \
  i_opens_the_chain_field_tab_completes_and_enter_writes_the_chain

# And the row itself: the labelled `name_row`, not a filter row with a
# search icon over a chain.
run_mutation "objectdialog: the chain field paints as the filter row" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    let filter = if draft.chain_entry {' \
  '    let filter = if false {' \
  geode-shell \
  i_opens_the_chain_field_tab_completes_and_enter_writes_the_chain

# The review's Major. Deriving the edit stage from `services.config`
# alone is exactly the old code: inside the debounce the draft hides the
# last tick and, outliving the flush, writes the object without it on the
# next one. Every single-entry fixture stays green — the stale window is
# only reachable by leaving and re-entering a slot inside 250 ms.
run_mutation "objectdialog: the edit stage ignores the pending batch" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    let folded = apply::config_with_pending(shell);' \
  '    let folded = apply::config_with_pending(shell).filter(|_| false);' \
  geode-shell \
  jumping_away_and_back_inside_the_debounce_keeps_the_queued_tick

# An inert apply must not touch the list. Skipping the early return makes
# the same chain typed back re-sort the list "typed names first" and
# report `Changed` for it — a write of an unchanged value at best, and a
# dirty draft under an answer of "inert" at worst.
run_mutation "groupings: an unchanged chain is applied anyway" \
  crates/geode-shell/src/shell/objectdialog/groupings.rs \
  '        if before == names {' \
  '        if false {' \
  geode-shell \
  applying_the_unchanged_chain_is_inert_and_closes_the_field

# The action bar has to be withdrawn while the chain field is open, or a
# clicked `d`/`r` arms a confirm over a live, focused value field. The
# keyboard tests cannot see it (no key reaches those verbs there); only
# the painted-bar assertion can.
run_mutation "objectdialog: the action bar stays up under the chain field" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '        (true, _) => div().into_any_element(),
        (false, Some(confirm)) => confirm_row(confirm, &draft.name, entity, cx),
        (false, None) => action_bar(shell, entity, cx),' \
  '        (_, Some(confirm)) => confirm_row(confirm, &draft.name, entity, cx),
        (_, None) => action_bar(shell, entity, cx),' \
  geode-shell \
  i_opens_the_chain_field_tab_completes_and_enter_writes_the_chain

# The landing is keyed on the domain (§18.8, user ruling 2026-09-12).
# Inverting it opens Views' and Scopes' edit stages in a chain field that
# has no list to complete against, and lands Groupings in the chooser —
# every Groupings test that `escape`s first still passes (an `escape` in
# the chooser merely steps back to browse and the next keystrokes act
# there, which most fixtures would notice, but only the one asserting
# BOTH domains' landings names the rule).
run_mutation "objectdialog: the chain-field landing ignores the domain" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        let opens_in_chain_field = self.domain == Domain::Groupings;' \
  '        let opens_in_chain_field = self.domain != Domain::Groupings;' \
  geode-shell \
  a_groupings_slot_opens_in_the_chain_field_and_a_view_does_not

# ---- Mouse parity (spec §17.1 rule 2): a browse row click opens --------

# §17.1 rule 2 on browse: a click opens. Mutated to select-only, the
# stage stays Browse under the click.
run_mutation "objectdialog: a browse row click opens the edit stage" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    if opens {
        enter_edit_stage(shell, &name, None, cx);
    }' \
  '    if opens && false {
        enter_edit_stage(shell, &name, None, cx);
    }' \
  geode-shell clicking_a_browse_row_opens_its_edit_stage

# The naming exception: a stray click must not discard the typed name.
run_mutation "objectdialog: a click while naming only selects" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    let opens = state.stage != Stage::Naming;' \
  '    let opens = true;' \
  geode-shell clicking_a_browse_row_while_naming_only_selects

# ---- Mouse parity Task 4 (§18.9): dropping a list row by name ---------

# §18.9.3: a drop takes the target's index. Mutated to append, every
# reorder lands at the end.
run_mutation "objectdialog: a drop takes the target row's index" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '                let entry = items.remove(src_ix);
                items.insert(dst_ix, entry);' \
  '                let entry = items.remove(src_ix);
                items.push(entry);' \
  geode-shell a_drop_takes_the_target_rows_index

# Available → Item places rather than appends.
run_mutation "objectdialog: an available row dropped onto the list is placed, not appended" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '                entry.included = true;
                items.insert(dst_ix, entry);' \
  '                entry.included = true;
                items.push(entry);' \
  geode-shell dropping_an_available_row_onto_the_list_adds_it_at_that_index

# Item → Available unticks on the way out, as `x` does.
run_mutation "objectdialog: a row dropped into the catalogue is unticked" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '                let mut entry = items.remove(src_ix);
                entry.included = false;' \
  '                let mut entry = items.remove(src_ix);' \
  geode-shell dropping_an_item_onto_the_catalogue_removes_it

# Same row is inert: without the guard a self-drop is a remove+insert
# that dirties nothing but still reports Changed and queues a write.
run_mutation "objectdialog: a drop on its own row is inert" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        if src_row == dst_row {
            return Step::Inert;
        }' \
  '        if src_row == dst_row && false {
            return Step::Inert;
        }' \
  geode-shell inert_drops_change_nothing

# The cursor follows the dropped item.
run_mutation "objectdialog: the cursor follows a dropped item" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        self.follow(landed);
        Step::Changed' \
  '        let _ = landed;
        Step::Changed' \
  geode-shell the_cursor_follows_the_dropped_item

# `locate` resolves a payload by NAME, not by whatever index the row
# happened to be at when it was dragged. Mutated to a positional
# resolve, every payload lands on the list's first row instead.
run_mutation "objectdialog: locate resolves a drag payload by name" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        let item = list.iter().position(|i| i.name == drag.name)?;' \
  '        let item = 0;' \
  geode-shell row_drag_round_trips_through_locate

# ---- Mouse parity Task 5 (§18.9.2): the tick is the toggle -------------

# §18.9.2: the tick toggles. Mutated to a bare select, a tick click moves
# the cursor and changes nothing.
run_mutation "objectdialog: a tick click toggles through space's path" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    draft.selected = position;
    match draft.toggle_selected() {' \
  '    draft.selected = position;
    match Step::Inert {' \
  geode-shell clicking_a_tick_hides_the_column_and_parks_the_cursor_there

# Review finding on Task 5: a tick click must be claimed and dropped
# while a confirm is armed, exactly as `handle_edit_key`'s bare-letter
# case is — otherwise it can act on the object behind a pending
# Delete/Revert/Fork. Mutated away, the guard never fires.
run_mutation "objectdialog: a tick click is claimed and dropped while a confirm is armed" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    if draft.confirm.is_some() {
        return;
    }
    if position >= draft.visible_rows().len() {' \
  '    if false && draft.confirm.is_some() {
        return;
    }
    if position >= draft.visible_rows().len() {' \
  geode-shell a_tick_click_does_nothing_while_a_confirm_is_armed

# Review finding: the tick's own mouse-down must stop propagation, or the
# same click falls through to the row's on_mouse_down (on_edit_row_clicked,
# which carries no confirm guard) and moves the cursor to the tick's row.
# Mutated away, the tick click still does nothing to the OBJECT while a
# confirm is armed (on_tick_clicked's own guard still holds), but it now
# also moves `draft.selected` — the half of the behaviour only this entry
# covers.
run_mutation "objectdialog: the tick's click does not stop propagation" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '.on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                            cx.stop_propagation();
                            entity_for_tick.update(cx, |shell, cx| {' \
  '.on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                            entity_for_tick.update(cx, |shell, cx| {' \
  geode-shell a_tick_click_does_nothing_while_a_confirm_is_armed

# ---- Mouse parity Task 6 (§18.9.1, §18.9.3): dragging a row --------------

# §18.9.1: the payload is by NAME. Mutated to ignore the source and act
# on the cursor's row, a drop moves whichever row is selected — which in
# the test is the TARGET, so the list comes back untouched.
run_mutation "objectdialog: a drop resolves its source by name, not the cursor" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    match draft.drop_row(src, dst) {' \
  '    match draft.drop_row(&draft.row_drag(draft.selected_row().unwrap_or(EditRow::Field(0))).unwrap_or_else(|| src.clone()), dst) {' \
  geode-shell the_drop_handler_reorders_and_writes

# The same guard Task 5 put on the tick, on the other handler that
# reaches the row list: a drop must not act behind an armed confirm.
# Mutated away, it clobbers the pending Delete and writes.
run_mutation "objectdialog: a drop is claimed and dropped while a confirm is armed" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    if draft.confirm.is_some() {
        return;
    }
    let resolves' \
  '    if false && draft.confirm.is_some() {
        return;
    }
    let resolves' \
  geode-shell a_row_drop_does_nothing_while_a_confirm_is_armed

# §18.9.3: a catalogue-to-catalogue drop SAYS there is no order there.
# Mutated silent, the gesture is indistinguishable from a missed drop.
run_mutation "objectdialog: a catalogue-to-catalogue drop says so" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '            set_notice(shell, "the catalogue has no order".to_string());' \
  '            let _ = shell;' \
  geode-shell a_catalogue_to_catalogue_drop_says_the_catalogue_has_no_order

# §18.9.1: a payload whose name left the list mid-drag says so rather
# than landing silently on nothing. Mutated silent, it is invisible.
run_mutation "objectdialog: a drop on a name that is gone says so" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '        Step::Inert if !resolves => set_notice(shell, "that row is gone".to_string()),' \
  '        Step::Inert if !resolves => {}' \
  geode-shell a_drop_whose_name_has_left_the_list_says_that_row_is_gone

# Controller ruling M5: a row dropped on ITSELF is silent from either
# block, which is why it is decided before the catalogue arm. Mutated
# away, an available row put back where it was says "the catalogue has
# no order".
run_mutation "objectdialog: a row dropped on itself is silent" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '        Step::Inert if src == dst => {}' \
  '        Step::Inert if false && src == dst => {}' \
  geode-shell a_catalogue_to_catalogue_drop_says_the_catalogue_has_no_order

# ---- Mouse parity Task 7 (§18.9.4): a completion row click completes --

# §18.9.4: a completion click completes. Mutated to a bare select, the
# click moves the highlight and the field's text is unchanged.
run_mutation "objectdialog: a completion row click completes the chain" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '        draft.selected = position;
        draft.complete_chain()' \
  '        draft.selected = position;
        true' \
  geode-shell clicking_a_completion_row_completes_the_chain

# ---- Mouse parity: the command palette (2026-09-12) ------------------
# 2026-09-12: a palette row click is the mouse form of enter — select the
# clicked row, then commit through the same door the key uses. Mutated
# back to the old select-only click, the palette stays open and the
# clicked row's action never runs.
run_mutation "palette: a row click dispatches the clicked row" \
  crates/geode-shell/src/shell/render.rs \
  '                        view.commit_selected(window, cx);
                        cx.notify();' \
  '                        view.sync_palette_scroll();
                        cx.notify();' \
  geode-shell click_on_a_result_row_dispatches_it_like_enter
# ---- Settings dialog goes modal (interaction-model spec §18, 2026-09-12) --
#
# The same four silent failures spec §12 lists for the keybinding dialog,
# now on the settings surface. Each keeps every filter-mode test green —
# the whole pre-modal suite still passes with the dialog opening
# filter-first, since every one of those tests now presses `/` first or
# uses keys both modes share.

# The opening mode is one line. Only a test asserting a bare letter did
# NOT reach the filter can see it.
run_mutation "settings: the dialog opens in filter mode" \
  crates/geode-shell/src/shell/settings_view.rs \
  '            mode: DialogMode::Normal,' \
  '            mode: DialogMode::Filter,' \
  geode-shell \
  the_settings_dialog_opens_in_normal_mode_and_letters_do_not_type

# `EscapeStep::LeaveFilter` keeps the query APPLIED. A dialog that
# cleared it on the way out still walks the same number of rungs.
run_mutation "settings: leaving filter mode clears the query" \
  crates/geode-shell/src/shell/settings_view.rs \
  '            state.mode = DialogMode::Normal;' \
  '            state.mode = DialogMode::Normal;
            state.query.clear();' \
  geode-shell \
  settings_escape_walks_the_ladder_one_rung_at_a_time

# The ladder's second rung skipped: escape with a query applied closes
# the dialog and loses the filter with it.
run_mutation "settings: escape skips the clear-query rung" \
  crates/geode-shell/src/shell/settings_view.rs \
  '            EscapeStep::ClearQuery => KeyAction::ClearQuery,' \
  '            EscapeStep::ClearQuery => KeyAction::PassThrough,' \
  geode-shell \
  settings_escape_walks_the_ladder_one_rung_at_a_time

# Spec §11 risk 3: `space` stepping in FILTER mode changes a setting under
# a trader typing a two-word query. Every normal-mode step test passes.
run_mutation "settings: space steps in filter mode too" \
  crates/geode-shell/src/shell/settings_view.rs \
  '        DialogMode::Filter => KeyAction::PassThrough,' \
  '        DialogMode::Filter if ks.key != "space" => KeyAction::PassThrough,
        DialogMode::Filter => KeyAction::Step(StepDirection::Right),' \
  geode-shell \
  space_types_in_settings_filter_mode_rather_than_stepping

# `space` as text is `PassThrough`; a handler that swallowed every
# unclaimed filter-mode key would keep the value unchanged (the only
# thing the step test checks) while typing nothing — the list would
# never narrow past the first word.
run_mutation "settings: filter mode drops printable keys instead of passing them" \
  crates/geode-shell/src/shell/settings_view.rs \
  '        DialogMode::Filter => KeyAction::PassThrough,' \
  '        DialogMode::Filter => KeyAction::Drop,' \
  geode-shell \
  space_types_in_settings_filter_mode_rather_than_stepping

# The sync's settings arm: without it `/` flips the mode and nothing
# moves focus, so typing falls on the floor under a pill reading
# `filter`. The open test cannot see this — the door passes
# `focus_filter: false`, so a sync that ignores settings leaves the
# field blurred exactly as normal mode would.
run_mutation "dialog: sync_dialog_text ignores the settings dialog" \
  crates/geode-shell/src/shell/dialog.rs \
  '    } else if let Some(state) = shell.settings.as_ref() {' \
  '    } else if let Some(state) = shell.settings.as_ref().filter(|_| false) {' \
  geode-shell \
  slash_enters_settings_filter_mode_and_typing_narrows

# §17.1 rule 1 on this surface: the frozen row's click must reach the
# settings state, or the row paints as typeable and does nothing.
run_mutation "dialog: enter_filter_by_mouse ignores the settings dialog" \
  crates/geode-shell/src/shell/dialog.rs \
  '    } else if let Some(state) = shell.settings.as_mut() {' \
  '    } else if let Some(state) = shell.settings.as_mut().filter(|_| false) {' \
  geode-shell \
  clicking_the_settings_frozen_filter_row_enters_filter_mode

# ---- schema: the document family (market-data spec §3)
#
# `from_doc`'s new dataset-level reads (family, key, axes) and the
# `ColumnRole::Attribute` grain split they drive downstream.

# An unknown `family` must drop the whole dataset, not fall back to
# Measures silently: a dataset a trader named `document` and misspelled
# would otherwise parse as an ordinary (and wrong) measure dataset with
# no diagnostic pointing at the typo.
run_mutation "schema: an unknown family drops the dataset rather than defaulting to measures" \
  crates/geode-core/src/schema/mod.rs \
  '                        continue;' \
  '                        Family::Measures' \
  geode-core \
  an_unknown_family_is_an_error_and_the_dataset_is_dropped

# `key`/`axes` on the measure family must be cleared, not merely
# warned about and left in place -- a stray `key = [...]` on a measure
# dataset must never reach `DatasetSpec.key` where nothing reads it
# today but Task 2's validator will.
run_mutation "schema: key/axes on the measure family are cleared, not left in place" \
  crates/geode-core/src/schema/mod.rs \
  'if family == Family::Measures {' \
  'if false {' \
  geode-core \
  a_measure_family_key_and_axes_are_cleared_with_a_warning

# `create_table_sql`'s grain-path `keep` match must treat a document-level
# attribute, an axis, and a value the same as a key or a bare dimension:
# never selected into a measure grain's table. Nothing in today's schema
# reader can put these roles on a measure dataset, so only a
# hand-built fixture (this test) can see the arm at all.
run_mutation "ddl: create_table_sql never selects an axis, value, or document-level attribute" \
  crates/geode-data/src/store/ddl.rs \
  '            | ColumnRole::Axis
            | ColumnRole::Value => false,' \
  '            | ColumnRole::Axis
            | ColumnRole::Value => true,' \
  geode-data \
  create_table_never_selects_a_document_shaped_column

# `payload_columns`' wildcard arm is the same exclusion, collapsed to one
# `_ => false`: an axis, a value, and a document-level attribute (along
# with every key and bare dimension) must never become ingest payload at
# any grain.
run_mutation "split: payload_columns never selects an axis, value, or document-level attribute" \
  crates/geode-data/src/ingest/split.rs \
  '            _ => false,' \
  '            _ => true,' \
  geode-data \
  payload_columns_never_selects_a_document_shaped_column

# `attribute_conflicts` must count only an attribute carried AT this
# grain -- a document-level attribute (`grain: None`) has no grain to
# match and must never be folded in. Widening the guard to any
# `Attribute` at all would send the query at a table this dataset never
# created, since a document-level-only dataset has no grain tables.
run_mutation "catalog: attribute_conflicts ignores a document-level attribute" \
  crates/geode-data/src/store/catalog.rs \
  'matches!(c.role, ColumnRole::Attribute { grain: Some(g) } if g == grain)' \
  'matches!(c.role, ColumnRole::Attribute { .. })' \
  geode-data \
  attribute_conflicts_ignores_a_document_level_attribute

# ---- schema: validating a document dataset (market-data spec §3.2)
#
# Task 2's load-time rules for the document family, dispatched from
# `validate_dataset` into `validate_document`, plus the mirror refusal on
# the measure family.

run_mutation "schema/document: an empty key drops the dataset" \
  crates/geode-core/src/schema/mod.rs \
  '    if ds.key.is_empty() {' \
  '    if false {' \
  geode-core a_document_dataset_needs_a_non_empty_key_and_axes

run_mutation "schema/document: a key column must be a dimension" \
  crates/geode-core/src/schema/mod.rs \
  '            Some(c) if !matches!(c.role, ColumnRole::Dimension { grain: None }) => {' \
  '            Some(c) if false && !matches!(c.role, ColumnRole::Dimension { grain: None }) => {' \
  geode-core every_key_column_must_be_a_dimension

run_mutation "schema/document: an axis role not listed in axes is refused" \
  crates/geode-core/src/schema/mod.rs \
  '        .filter(|c| c.role == ColumnRole::Axis && !ds.axes.contains(&c.name))' \
  '        .filter(|c| false && c.role == ColumnRole::Axis && !ds.axes.contains(&c.name))' \
  geode-core axes_and_axis_roles_must_agree_both_ways

run_mutation "schema/document: measure vocabulary is dropped per column" \
  crates/geode-core/src/schema/mod.rs \
  '    ds.columns.retain(|c| !foreign.contains(&c.name));

    // A value is a number: it feeds the numeric cell of the document' \
  '    let _ = &foreign;

    // A value is a number: it feeds the numeric cell of the document' \
  geode-core measure_vocabulary_on_a_document_dataset_is_refused_per_column

run_mutation "schema/measures: document vocabulary is dropped per column" \
  crates/geode-core/src/schema/mod.rs \
  '    ds.columns.retain(|c| !foreign.contains(&c.name));

    // Every grain in use groups by its key columns' \
  '    let _ = &foreign;

    // Every grain in use groups by its key columns' \
  geode-core document_vocabulary_on_a_measure_dataset_is_the_mirror_error

run_mutation "schema/document: a refused dataset is not pushed" \
  crates/geode-core/src/schema/mod.rs \
  '            if dataset.is_document() && dataset.columns.is_empty() {' \
  '            if false {' \
  geode-core a_document_dataset_declares_at_least_one_value

# The document-family dispatch must fold the reserved-column diagnostics
# pushed above it into the return, not discard them by returning
# `validate_document(ds)` directly -- a document dataset naming a
# storage-layer column (`batch`, `source_file_id`, ...) would otherwise
# load with no warning and hit the exact DDL collision the check exists
# to prevent (review round 1's Important finding).
run_mutation "schema: the document dispatch keeps the reserved-column diagnostics" \
  crates/geode-core/src/schema/mod.rs \
  '        diags.extend(validate_document(ds));' \
  '        return validate_document(ds);' \
  geode-core a_reserved_column_name_on_a_document_dataset_is_still_a_diagnostic

run_mutation "schema/document: groupable columns are the dimensions, not the axes" \
  crates/geode-core/src/schema/mod.rs \
  '                .filter(|c| matches!(c.role, ColumnRole::Dimension { .. }))
                .map(|c| c.name.as_str())
                .collect();
        }' \
  '                .filter(|c| matches!(c.role, ColumnRole::Dimension { .. } | ColumnRole::Axis))
                .map(|c| c.name.as_str())
                .collect();
        }' \
  geode-core a_document_dataset_has_no_grain_and_groups_by_its_dimensions_only

run_mutation "schema/document: document_columns follows the declared axes order" \
  crates/geode-core/src/schema/mod.rs \
  '        out.extend(self.axes.iter().filter_map(|a| self.column(a)));' \
  '        out.extend(self.columns.iter().filter(|c| c.role == ColumnRole::Axis));' \
  geode-core document_columns_are_key_then_axes_then_values_then_attributes

if [[ -n "$changed_ref" ]]; then
  echo "skipped $skipped entries whose files are unchanged since $changed_ref"
fi
if (( anchors_only )); then
  # One pass: each file read once, every selected entry's anchor counted.
  # Non-zero on any finding so this can gate a merge (MIN-2 of its own
  # review); "nothing selected" is reported as such, never as a pass.
  python3 - "$anchors" <<'PY' || exit 1
import sys, pathlib
raw = pathlib.Path(sys.argv[1]).read_bytes() if pathlib.Path(sys.argv[1]).exists() else b""
fields = raw.split(b"\0")[:-1] if raw else []
entries = [tuple(f.decode() for f in fields[i:i + 3]) for i in range(0, len(fields), 3)]
if not entries:
    print("checked 0 anchors (nothing selected)")
    sys.exit(1)
texts = {}
stale = ambiguous = 0
for name, file, anchor in entries:
    if file not in texts:
        try:
            texts[file] = pathlib.Path(file).read_text()
        except OSError:
            texts[file] = None
    text = texts[file]
    if text is None:
        stale += 1
        print(f"ANCHOR    {name}  <-- file missing: {file}")
        continue
    hits = text.count(anchor)
    if hits == 0:
        stale += 1
        print(f"ANCHOR    {name}  <-- anchor no longer matches; mutation is stale")
    elif hits > 1:
        ambiguous += 1
        print(f"AMBIG x{hits}  {name}  <-- anchor matches {hits} times; only the first is mutated")
print(f"checked {len(entries)} anchors: {stale} stale, {ambiguous} ambiguous")
sys.exit(1 if stale or ambiguous else 0)
PY
fi
