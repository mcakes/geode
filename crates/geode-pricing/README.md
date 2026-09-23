# geode-pricing

Implementations of `geode_core::pricing::Pricer`. This is a calculation leaf:
it depends on shared vocabulary and is reached through requests rather than a
direct call from a UI module.

Current behavior and rationale:
[`docs/current/features.md`](../../docs/current/features.md#pricing-and-the-line-pricer).

## Current implementation

`MockPricer` produces deterministic values for tests and demo workflows. Its
numbers are shaped to respond plausibly to spot and volatility shifts but are
not a financial model. `FAIL` is a deliberate refused underlying used to test
per-line failure handling.

## Commands

```sh
cargo test -p geode-pricing
```

## Rules this crate pins

- A pricer receives market overrides once per batch through its stateful
  `set_overrides` seam.
- Invalid overrides are refused before pricing any line.
- The implementation has no dependency on the shell, data service, or a
  feature module.
