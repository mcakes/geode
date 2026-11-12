# Implied forward and volatility fitting process — design sketch

Nov 9, 2026 · @Matthew

## Overview

One Rust binary with five roles. The fit pool and the persistence writer are the only places that do real work; the rest move data between them.

&#91;embedded content: fitting process · 5 roles, 1 writer\]

Three principles hold it together: chain state has a single writer and is read through immutable snapshots; fitting is a pure function of a snapshot, so replay is deterministic; and the writer is the only thing that touches storage. Quotes are persisted as change events, not full-chain snapshots, and fit outputs are persisted at full cadence with the fitter version and config hash needed to reproduce them.

## The roles

### Feed handler

One thread, pinned to a core. Decodes the vendor/OPRA feed into a fixed-size `QuoteEvent { contract_id: u32, ts_exch, ts_recv, bid, ask, bid_sz, ask_sz, seq }`. Spot/futures, the SOFR curve, and dividend/borrow inputs arrive as separate small event types. Pushes into two bounded channels: one to chain state, one to the writer. It never blocks on either; a full writer channel is an alarm, not a drop.

### Chain state

Single writer. A struct-of-arrays indexed by `contract_id` holding latest bid/ask/sizes/timestamp plus a monotonically increasing version. Only the feed thread writes it. Readers get a consistent view via `arc_swap` double-buffering or a seqlock; the snapshot clock is the only hot-path reader, so a seqlock is enough. A `contracts` table (expiry, strike, type, root, settlement) loads at start and when new series list. `contract_id` is minted by the process, not taken from the vendor.

### Snapshot clock

A tokio interval aligned to wall clock (`:00`, `:02`, `:04`, ...). Copies the current chain state into an immutable `Arc<ChainSnapshot>` tagged with snapshot time and state version, and hands it to the fit pool. If the previous fit is still running, skip the tick and count it; never queue. Alignment makes snapshots comparable across underliers, which matters for anything cross-index.

### Fit pool

Rayon, one task per expiry. Pure functions over the snapshot: no I/O, no shared mutable state. This is what makes replay deterministic. Per expiry:

1. Filter: stale (ts older than threshold), crossed/locked, zero bid, relative spread above cap, outside a moneyness window.
2. Implied forward and discount: weighted regression of `C - P` on `K` across near-ATM strikes using put-call parity, weights from inverse spread. Solve jointly for `F` and `D` with enough strikes; otherwise pin `D` from the curve and solve `F` only. Flag when the result disagrees with futures-implied by more than a tolerance.
3. Implied vols from mids given `F` and `D`. Vectorized; the largest CPU cost on SPX and still milliseconds in Rust.
4. Fit the slice (SVI/SSVI/eSSVI or the chosen parametric form), warm-started from the previous snapshot's params. Butterfly check per slice; calendar check across the term structure once all expiries complete.
5. Diagnostics: RMSE in vol points, point count used, max residual and its contract, iteration count, wall time, warm-start distance.

A short term-structure step then runs across expiries: smooth the forward curve into an implied dividend/basis term structure, run calendar arbitrage checks, and emit the `FitResult` set for the snapshot.

### Publish

Latest `FitResult` per (underlier, expiry) into shared memory (`iceoryx2` or a memmap) for pricers, risk and UI. Replace-on-write; consumers never see a half-updated slice.

### Persistence writer

One dedicated thread that owns the DuckDB connection. Receives three channel streams: raw quote events, fit results, and fit inputs (the contract ids, mids and weights actually used per fit). Accumulates each into Arrow `RecordBatch` builders and flushes every 1-5 min or N rows via the Appender or Arrow ingest. Snapshots are not stored at full cadence; they are reconstructed from quote events with an as-of query. A `runs` table records each process start: version, config hash, mode, host.

## Tables

Seven DuckLake tables (`config_snapshots` is described under Configuration); `quotes` is the only large one.

| Table | Key columns | Notes |
| --- | --- | --- |
| `contracts` | `contract_id`, underlier, root, expiry (`Date32`), strike cents (`Int64`), c/p, am/pm settlement, listed date | Reference data; loaded at start, appended as series list |
| `quotes` | `contract_id`, `ts_exch`, `ts_recv`, bid, ask, bid\_sz, ask\_sz, seq | Events, not snapshots; partitioned by underlier and date, sorted by `ts_recv` |
| `underlying` | underlier, ts, spot, futures by expiry, curve points | Small; same cadence as the feed |
| `fits` | `fit_id`, snapshot ts, underlier, expiry, `F`, `D`, model name, params, diagnostics, status, `run_id` | Params as `STRUCT` or `LIST<DOUBLE>`; `VARIANT` if the model varies per expiry |
| `fit_inputs` | `fit_id`, `contract_id`, mid, iv, weight, used flag | What each fit actually consumed |
| `runs` | `run_id`, started, fitter version, config hash, mode, host | One row per process start |

Timestamps are `Timestamp(Nanosecond, UTC)` throughout. The Arrow schema is pinned in code and asserted on every batch; schema drift between files shows up weeks later in a research query otherwise.

## Configuration: universe and calibration profiles

Config is intent, written by people and changed rarely; it lives in git as TOML. What actually ran is a fact, and it lives in DuckLake as a materialized snapshot keyed by hash. Git holds what should run; the database holds what did.

| Kind of data | Medium | Format | Versioned by |
| --- | --- | --- | --- |
| Underliers to subscribe and calibrate | Git repo, `universe/` | One TOML file per underlier | Commit |
| Calibration profiles (hyperparameters) | Git repo, `profiles/` | One TOML file per named profile | Commit |
| Resolved config as run | DuckLake `config_snapshots` | JSON blob (`VARIANT`) plus hash and commit | `config_hash` |
| Listed contracts | DuckLake `contracts` | Table, from the vendor master | Append daily |
| Feed credentials | Environment or vault | Never in TOML | n/a |

### Layout

```
config/
  universe/
    spx.toml
    ndx.toml
    rut.toml
  profiles/
    index-default.toml
    index-0dte.toml
  env/
    prod.toml
    uat.toml
```

An underlier file names the feed symbology and which profile it uses. Profiles carry everything the fit pool reads. Per-tenor overrides live inside a profile so that 0DTE and long-dated expiries can filter and fit differently without a second underlier entry.

```toml
# universe/spx.toml
ticker = "SPX"
roots = ["SPX", "SPXW"]
feed_symbol = "SPX.OPRA"
futures_root = "ES"
settlement = { am = ["SPX"], pm = ["SPXW"] }
calendar = "CBOE"
active = true
profile = "index-default"

[overrides]
tenor_lt_2d = "index-0dte"
```

```toml
# profiles/index-default.toml
[filter]
stale_ms = 1500
max_rel_spread = 0.35
min_bid = 0.05
moneyness = { lo = -3.5, hi = 2.5 }   # in stdev units

[forward]
solve_discount = true
strike_window_pct = 0.03
weight = "inverse_spread"
futures_tolerance_bp = 15

[model]
kind = "essvi"
warm_start_damping = 0.5

[optimizer]
max_iter = 200
tol = 1e-9

[arbitrage]
butterfly_tol = 1e-6
calendar_tol = 1e-6
```

### Loading and validation

1. Deserialize with serde into typed structs, `#[serde(deny_unknown_fields)]`, every default explicit in code. A typo in a key fails startup, not a fit.
2. Resolve: apply `env/` overlay, then expand each underlier into a fully materialized per-tenor config with no references left.
3. Validate cross-field rules (moneyness window non-empty, tolerances positive, every referenced profile exists, every root in the vendor master).
4. Canonicalize the resolved config to JSON with sorted keys, hash it (BLAKE3), and write one row per distinct hash to `config_snapshots` with the git commit and timestamp. `runs.config_hash` points at it.

Every fit is then reproducible from the database alone, and questions like "which fits ran with `max_rel_spread` above 0.3" are a `VARIANT` query rather than a git archaeology exercise.

### Changing config while running

Profile edits reload on SIGHUP or a file watch; the process validates the new set, and if it passes, starts a new `run_id` with `parent_run_id` set and the new `config_hash`. Fits from the old and new config are never mixed under one run. Adding or removing an underlier changes subscriptions and memory layout, so that stays a restart.

In replay, `--profile <path>` overrides the profile for an experiment; the materialized snapshot is written the same way, so research runs sit next to production runs in the same tables and diff with the same SQL.

### Why not the alternatives

Config in DuckDB as the source of truth gives up review, diff, blame and rollback for a set of numbers that a trader will want to argue about. Environment variables do not scale past a dozen fields. YAML works but TOML is typed, comments cleanly, and maps directly onto serde structs. JSON has no comments, and the hyperparameters need them.

## Replay mode

Same binary with `--replay <from> <to>`. The feed handler is swapped for a reader that streams `quotes` and `underlying` in `ts_recv` order, the snapshot clock advances on event time, and everything downstream is unchanged. The writer tags results with a fresh `run_id`, so two fitter versions on the same day's data can be diffed in SQL. That diff, same inputs with old params against new, is the acceptance test for every fitter change.

## Budget check at 2 s

xThere is well over an order of magnitude of headroom, so spend it on diagnostics columns rather than speed.

| Step | Scale | Expected cost |
| --- | --- | --- |
| Snapshot copy | \~25k SPX contracts | Sub-millisecond |
| IV inversion | \~25k mids | Low single-digit ms |
| Slice fits | 50-60 SPX expiries, 8 cores | Tens of ms |
| Writer flush | Every 1-5 min | Off the fit path |

## Storage platform notes

DuckDB 2.0.0 LTS, released 21 October 2026 and supported until at least October 2027, is the target. The Rust client is at 2.0.0 (`duckdb = { version = "~1.20000.0", features = ["bundled"] }`), so the writer can bundle 2.0 directly. DuckLake 1.1 ships as a core extension and gives ACID appends, snapshot isolation and compaction over Parquet files on the share or object store. The storage format changed in 2.0; pin the version across the writer and every reader.

Two spikes before committing: confirm the quack/`CONNECT` client path works from the Rust crate for shared readers, and that the DuckLake extension loads under `bundled` on the target OS.

Hand-rolled Parquet via `arrow-rs` + `object_store` remains an option if the archive must outlive any single engine; it is a preference, not a necessity.

Sources: [Announcing DuckDB 2.0.0 LTS](https://duckdb.org/2026/10/21/announcing-duckdb-200) · [DuckDB 2.0 Rust client](https://duckdb.org/docs/2.0/clients/rust/overview) · [What's new in the docs for 2.0](https://duckdb.org/docs/2.0/whats_new_in_the_docs)
