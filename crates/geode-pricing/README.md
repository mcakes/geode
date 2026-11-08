# geode-pricing

Implementations of `geode_core::pricing::Pricer`, `geode_core::vol::VolModel`
and `geode_core::calibration::Calibrator`. This is a calculation leaf: it
depends on shared vocabulary and is reached through requests rather than a
direct call from a UI module.

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
the forward log-linear. Outside the document's terms it extrapolates:
the end term's smile holds flat in vol at equal moneyness, and the forward
keeps the nearest pair's log-linear carry (from `spot_ref` at time zero to
the first term before it, the last two terms past it, the end term's
forward when there is no pair). It floors vol at `0.01`, places points by
Black call delta and returns a Breeden–Litzenberger density unclamped,
per unit of the requested coordinate (`NaN` where delta saturates). A
`Grid::Dense` spans a term's node ladder (between terms, the union of the
two ladders), widened to the request's `cover`, its points at
`F·(1 + c·sinh(u))` with `u` even and `c` the at-the-money σ√t (floored at
a fiftieth of the span), so they crowd the forward; past the ladder the smile
continues the spline's end slope, floored, and a cover that is not an
ascending pair of positive strikes fails the job. It refuses a
`Grid::Job`, which only the vol worker can resolve.

`black` holds the undiscounted Black call price and delta over Hart's
double-precision normal CDF (about 1e-14 absolute near the money, exactly
0.5 at zero). The density is a second difference of these prices, which
divides a price error by the square of the strike step, so a
single-precision CDF would show as noise on a fine grid.
Any other `VolModel` implementation registered by `geode-app` in
`geode-data`'s `VolModelRegistry` is selected by name through `[vol] model`.

## Calibrators

`DemoForwardCalibrator` (`DEMO_CALIBRATOR`, `"demo"`) implements
`geode_core::calibration::Calibrator` over a `calibration.toml` pipeline. It
is a stand-in with the real engine's shape and demo arithmetic, documented
stage by stage at the top of `demo_calibration.rs`: `parity_fit` takes each
expiry's forward from the chain's `forward` attribute moved by a residual
scaled from the bid-ask widths inside the strike window (the demo chain has
no call/put split to regress), drops an expiry with fewer than `min_strikes`
strikes in the window and names it in the summary; `implied_rates` derives
`repo` and `box` from the forward, `spot_ref`, the dividend amounts and the
funding curve; `kalman_fit` is a real linear Kalman update of the knot values
(`demo_calibration/kalman.rs`), started by least squares on the first run;
`rasterize` reads the fit on the 60-tenor `monthly_to_5y` grid; `publish`
answers one `rate_curve` document per series. A `Run` from a middle stage, an
`Evaluate` and a `Publish` read the last payload of every stage upstream of
what they run; `Evaluate` writes nothing. Filter state and those payloads
live in memory behind one mutex, keyed by pipeline, underlying and stage, and
are lost on restart. So the held publish is reachable on demo data, the
`monotone_df` check reports one forced violation at the second tenor on runs
7 and 8 of every 14 for an underlying (`FORCED_VIOLATION_RUNS` and
`FORCED_VIOLATION_CYCLE`), and `Publish` is refused while the last run's
publish stage is held. Nothing is seeded from the clock; the same request
sequence on a fresh engine answers the same numbers.

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
- The demo calibrator's `Evaluate` never writes filter state, and a `Publish`
  is refused while the last run's publish stage is held.
- The implementation has no dependency on the shell, data service, or a
  feature module.
