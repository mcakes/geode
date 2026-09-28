# geode-demo-data

Seeded risk batches, CSV fixtures, and synthetic CVI and dividend documents.
Risk batches use one vector per column; document generators return
`geode_core::document::DocumentRows`. The only workspace dependency is
`geode-core`. The application supplies document serialization and publishing
through `geode-documents` and the data service.

See [demo and application composition](../../docs/current/features.md#demo-and-application-composition)
for startup, caching, and ingestion ownership.

## Modules

| Module | Responsibility |
|---|---|
| `generate` | Risk structure and measures assigned at position, instrument, underlying, and pair grains. `demo_underlyings` supplies the shared risk/document vocabulary. |
| `model` | `RiskBatch`, canonical column groups, and typed column access. Each row names an ordered underlying pair for an instrument. |
| `emit` | CSV header mapping, fixed-factor USD twins, file partitioning, sentinels, and deliberate missing-column and conflict fixtures. |
| `documents::cvi` | Fixed CVI axes and per-key walks for node parameters, ATM volatility, and skew. |
| `documents::dividend` | Per-key schedules with repeated dates, stable generated IDs, amount walks, status promotions, and appended rows. |

## Risk and file fixtures

Risk generation repeats coarse-grain values across finer rows. Underlying
measures belong to an instrument and underlying; both orderings of a pair
share cross-gamma values. Ingestion must deduplicate at the declared grain
before aggregating. Position counts scale with the requested batch size while
book and LHU cardinalities stay fixed.

Files are grouped by date and book. `BK000` is split on position boundaries;
`BK001` and `BK002` share a file. A position stays in one file because
splitting it across ingestion partitions would double-count its measures.
The emitter deliberately exercises ingestion boundaries:

- Every third file omits `skew01`, `rho_ois010`, and their USD twins.
- Alternate rows of the first instrument in the second file receive a
  conflicting model code, exposing disagreement within a grain group.
- Source times vary by file to exercise freshness reporting.
- `EmitOptions::new` withholds the final file's readiness sentinel by default.

USD twins use a fixed factor of 1.08, independent of the currency column.
CSV strings come from fixed, comma-free vocabularies; the writer performs
no quoting or escaping. Files are written directly, with I/O errors returned
to the caller; emission is not an atomic directory replacement. Use a fresh
output directory when relying on missing sentinels: withholding one does not
remove a sentinel from an earlier run.

`GeneratorConfig` is a fixture configuration, not a calendar model. Date
slots start at `2026-08-24` and increment the day field without month rollover
or business-day validation. Zero date slots uses one; the row limit can stop
generation before later slots are visited or partway through an instrument's
pairs. A zero row target currently emits one row.

## Document sequences

Both document generators seed independent RNG state per key. The same seed,
anchor date, and calls for a key reproduce its sequence, regardless of calls
for other keys. They retain a fixed anchor date throughout their lifetime.

CVI documents contain twelve fixed nodes at eight monthly third-Friday
expiries on or after the anchor. Rows are term-major. Spot, carry, forward,
and axes stay fixed; node parameters, ATM volatility, and skew walk on each
subsequent call. ATM and skew remain within their configured bounds.

Dividend schedules use 30–40 rows with two or three same-ex-date pairs for
`SPX`, `NDX`, and `RUT`. Other keys use 8–12 quarterly rows and may add one
special dividend. Initial status is paid for past ex-dates, declared from the
schedule date through 30 days ahead, and estimated beyond that, with a 5%
cancellation override. Each republish walks one or two amounts, floored at
0.01; every third promotes the nearest estimated row and every fifth appends
a future row. Output is sorted by ex-date and generated ID. Existing IDs and
dates remain stable inside the generator; wire serialization and parse-time
row identity belong to `geode-documents`.

## Demo configuration

The application loads [examples/demo-config](../../examples/demo-config) below
desk and user configuration. Its files declare:

- `datasets.toml`: risk column names, roles and grains, document schemas,
  and the shared series cache.
- `views.toml`: the default `tree` view and a 100-column `wide` view.
- `dimensions.toml` and `groupings.toml`: book-to-desk mapping and grouping slots.
- `app.toml`: blotter staleness, the default series source, and pricer
  underlying suggestions matching `demo_underlyings`.

`geode --demo [rows]` caches source files and its database under
`$TMPDIR/geode-demo/<rows>-42/`. Schema changes require clearing that directory;
existing payload tables are not migrated automatically.

## Commands

```sh
cargo test -p geode-demo-data
cargo doc -p geode-demo-data --no-deps
cargo bench -p geode-demo-data
cargo run -p geode-demo-data --example emit -- <dir> [rows]
```

The `generate` benchmark measures fresh 100,000- and 1,000,000-row batches.
The `dividend` benchmark measures initial schedule creation and the first
republish for index and regular keys. Republish setup is excluded from timing;
that call walks amounts but does not reach the promotion or append cadence.
