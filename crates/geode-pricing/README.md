# geode-pricing

Implementations of `geode_core::pricing::Pricer` and
`geode_core::vol::VolModel`. This is a calculation leaf: it depends on shared
vocabulary and is reached through requests rather than a direct call from a UI
module.

Current behavior and rationale:
[`docs/current/features.md`](../../docs/current/features.md#pricing-and-the-line-pricer).

## Current implementation

`MockPricer` produces deterministic values for tests and demo workflows. Its
numbers are shaped to respond plausibly to spot and volatility shifts but are
not a financial model. `FAIL` is a deliberate refused underlying used to test
per-line failure handling.

A request names its payout currency and the mock prices in it, with no quanto
adjustment. Each `_usd` value is the local value at the currency's rate in
`usd_rate`: USD 1.0, EUR 1.08, JPY 0.0067, GBP 1.27, CHF 1.12, HKD 0.128,
KRW 0.00073. A currency outside that table is refused with
`no USD rate for {code}` rather than priced at an invented rate; the table
covers every currency the demo reference data names.

## Vol models

`DemoVolModel` implements `geode_core::vol::VolModel` for `cvi_params`
documents. It is a stand-in, not a model: a natural cubic spline in
moneyness through knots the node ladder places (`node/100`), lifted by
`atm + skew·k + param/100`, total variance linear in time between terms,
the forward log-linear. It refuses an expiry outside the document's
terms rather than extrapolating, floors vol at `0.01`, places points by
Black call delta and returns a Breeden–Litzenberger density unclamped,
per unit of the requested coordinate (`NaN` where delta saturates). It
refuses a `Grid::Job`, which only the vol worker can resolve.
Any other `VolModel` implementation registered by `geode-app` in
`geode-data`'s `VolModelRegistry` is selected by name through `[vol] model`.

## Commands

```sh
cargo test -p geode-pricing
```

## Rules this crate pins

- A pricer receives market overrides once per batch through its stateful
  `set_overrides` seam.
- Invalid overrides are refused before pricing any line.
- A result is in the requested currency or the line is refused; the mock
  never substitutes another currency.
- The implementation has no dependency on the shell, data service, or a
  feature module.
