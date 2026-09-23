# geode-demo-data

Deterministic synthetic risk data at the desk's real grain, plus synthetic
market-data documents. Seeded: the same config always yields identical
data. Struct-of-arrays throughout, no row objects.

It depends on `geode-core` alone. It produces `DocumentRows` and never
writes XML; turning rows into wire bytes is `geode-documents`' job.

Current behavior and rationale:
[`docs/current/features.md`](../../docs/current/features.md#demo-and-application-composition).

## What lives here

| Module | Holds |
|---|---|
| `generate` | Seeded generation: structure first (desks, books, LHUs, positions, instruments, underlyings), then measures assigned at their own grain so coarse values repeat exactly. That repetition is what the ingest grain split and conflict detector are tested against. `demo_underlyings` is the underlying vocabulary the demo bus reuses so its documents match the risk data. |
| `model` | `RiskBatch`, the struct-of-arrays batch at the source file's grain. |
| `emit` | Writes a realistic source directory: per-book CSVs in the source's own column spelling, each with a `.done` JSON sentinel. |
| `documents` | Synthetic CVI documents for the demo bus. |

## Commands

```sh
cargo test -p geode-demo-data
cargo test -p geode-demo-data same_seed
cargo bench -p geode-demo-data                                         # the generator
cargo run -p geode-demo-data --example emit -- <dir> [rows]            # write a source directory by hand
```

`geode --demo [rows]` calls the same emitter into
`$TMPDIR/geode-demo/<rows>-42/src`.

## Rules this crate pins

- `write_csv` does no quoting or escaping. Every string column draws from
  a fixed, comma-free vocabulary; revisit if a vocabulary ever grows
  free-form values.
- `geode-data` dev-depends on this crate for its benches and fixtures, so
  nothing here may depend on `geode-data`.
