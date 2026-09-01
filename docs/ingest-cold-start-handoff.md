# Cold start: parallel staging — handoff

Written 2026-09-01. **Nothing here is implemented.** The work is on hold
until there is real-world measurement; this file exists so whoever picks
it up does not have to rediscover what was already found.

Assume no memory of the conversation that produced this. Everything you
need is here or named here.

## What is decided, and what is not

`docs/perf.md` settles the *approach*: move staging onto a worker pool and
keep publishes serialized (DuckDB is single-writer, spec §5.3). It
measures 1.87× at 118k rows/file and calls this the next cold-start
optimization. That much is not in question.

What is **not** decided is whether the win survives contact with the real
staging path, and whether it survives a real filesystem. Two separate
reasons to measure before writing code.

## Reason 1: the benchmark measures a narrower operation than the change

**Verified by reading `crates/geode-data/benches/ingest.rs`.** The
parallel-staging benchmark stages a file like this, and only this:

```rust
fn stage_one(conn: &duckdb::Connection, csv: &Path, n: usize) {
    let sql = format!(
        "create or replace table bench_stage_{n} as
         select * from read_csv('{}', header = true)", …);
    conn.execute_batch(&sql).unwrap();
}
```

That is `read_csv` alone. The real staging path in
`crates/geode-data/src/ingest/load.rs` is `read_csv` into `staging_raw`
**and then** `split_by_grain` — a grouped aggregation per grain, four of
them on the desk schema, each an `any_value(...) … group by` over the raw
table (`crates/geode-data/src/ingest/split.rs`).

So **1.87× describes read_csv, not staging.** The split is a real share
of staging time and is absent from the number.

Whether the combined path parallelises as well is **unmeasured, and the
open question**. DuckDB's group-by is itself multi-threaded, which is the
same reason the original plan predicted (wrongly, for `read_csv`) that
parallelism would not pay. It may be right about the split even though it
was wrong about the read. Do not assume either way.

**Do this first:** extend `benches/ingest.rs` so the staged unit is
`read_csv` + `split_by_grain` rather than `read_csv` alone, and compare
sequential against parallel at 100k and 1M rows. If the combined speedup
is well under 1.87×, the added concurrency in the runner may not be worth
its complexity, and the other lever (below) becomes the better buy.

## Reason 2: the numbers are local SSD

`docs/perf.md` already flags this and it has not been done: the desk reads
from a network share, and every cold-start number in that file was taken
on a local SSD. Parallelism across files is exactly the kind of win that
changes character when the bottleneck moves from CPU to network latency —
it could get *better* (latency hides under concurrency) or worse
(contention on one mount). Re-measure on a real share before trusting any
of it.

## What you will hit when you implement it

Found while reading the code, not by building anything. These are the
things that make it more than "wrap the loop in a thread pool".

1. **Staging table names are fixed and global.** `load.rs` has
   `const RAW_TABLE: &str = "staging_raw"`, and `split.rs` has
   `staging_table(grain) -> format!("staging_{}", grain.table())`. Both
   are created with `create or replace table`. **Two files staged
   concurrently would silently overwrite each other's staging tables.**
   Parallel staging requires per-file unique names, which changes a
   shared invariant several call sites read.

2. **Preemption granularity changes.** The runner re-sorts its queue on
   every submit so a newly landed current file jumps ahead of remaining
   backfill without interrupting the load in flight (spec §5.4, and the
   module doc in `runner.rs` says so explicitly). With N files staging
   concurrently, worst-case latency before a hot file *starts* grows from
   one file to N. Decide deliberately whether that is acceptable, and say
   so in the runner's module doc either way — it currently documents the
   one-thread choice as deliberate, and that comment will be wrong.

3. **Where `gen_id` and `file_id` get reserved.** `load_file` currently
   reserves both up front (`Catalog::reserve_gen_id`,
   `Catalog::reserve_file_id`), then stages, then publishes. If staging
   is parallel and publishes serial, files can publish in a different
   order than they reserved. `gen_id` comes from a DuckDB sequence now,
   so ids stay unique and monotonic in *allocation* order — but that is
   no longer publish order. Decide whether ids should be reserved at
   publish time instead, and check what depends on the two agreeing.

4. **Concurrent staging tables cost space.** N files' worth of raw plus
   per-grain staging tables live in the database at once. At 1M rows per
   file that is not free. Bound the in-flight count rather than spawning
   per file.

5. **How the benchmark sidesteps the single writer.** Each worker gets a
   connection from `store.reader()`, which is a `try_clone` of the writer
   — a separate connection to the same in-process database — and writes
   its own staging table through it. Distinct table names are what keeps
   that safe. Publishes must still serialize through the one writer
   (§5.3); only staging fans out.

## The other lever, which is independent

`docs/perf.md` decomposes per-file cost as roughly `111 ms + 10.3 µs ×
rows`. Parallel staging attacks the per-row term and pays on the
large-file case a normal day produces. **Batching the publish across
grains into one transaction** attacks the 111 ms fixed cost and pays on
many small files — a multi-day backfill, where file count dominates.

They are independent and cold start wants both. If the measurement in
Reason 1 comes back weak, do the publish-batching one first: it is a
smaller change, it does not touch the threading model at all, and its
target case is the one where cold start hurts most.

## Verification expectations

Same rules as the rest of this repo, and they are not optional here:
`zsh scripts/mutation-check.sh` before and after, an entry per behaviour
changed, and no claim about a speedup that was not measured on the path
that actually changed. The 1.87× in `docs/perf.md` is precisely the kind
of number this codebase has been bitten by — it is real, it is just about
a different operation than the one you would be modifying.
