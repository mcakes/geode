# Historical feature ideas

This undated planning inventory mixes implemented features with proposals.
It is retained as a record of ideas, not as a description of current behavior
or a committed roadmap. See [current features](current/features.md) and
[architecture](current/architecture.md) for supported capabilities and ownership.

## Blotter
- Input: 
    * risk_snapshot, trade_blotter, daily_pnl Datasets
    * Scope (from shell)
- Outputs: 
    * Scope (based on where active row is)


## Scenario Panel
- Aggregated risk grids.
    - Vega K/T Matrix (T x K)
    - Vega by Maturity Spot Ladder (T x S)
    - Spot Ladder (S x NPVs, greeks)
    - By Maturity
        - Repo by Maturity (T)
        - ATMVol by Maturity (T)
        - Skew by Maturity (T)
        - Curvature by Maturity (T)
        - Repo by Maturity (T)
    - Rho by `{ccy} {curve_type}` by Maturity (T x M)
- Input: 
    * bucket_vega, rho_maturity, vega_maturity_spot_ladder, spot_ladder, repo_maturity, vega_maturity, skew_maturity, curvature_maturity Datasets
    * Scope (should be able to be driven by blotter scope or global scope or pinned at some specific scope)

## Watchlist editor
- Named managed list of underlyings
- Input: 
    * User text
    * Dataset

## Vol Watchlist
- Underlyings in Watchlist and vol metrics
- Metrics: Underlying | 5d Rlz | 20d Rlz | 1M ATFM | 3M ATFM | 6M ATFM | 1Y ATFM | 18M ATFM | 2Y ATFM | 3Y ATFM
- Input: 
    * KDB (Hanweck)
    * Reuters
    * Bloomberg
    * Underlying list

## CVI Params
- Tabular display of CVI params (T x N + Expiry) and metadata
- Input: XML via Solace
- Output: Upload to Sophis

## Repo Curve
- Tabular display (T x 1 +Expiry) of repo curve and metadata
- Input: XML via Solace
- Output: Upload to Sophis


## Dividend Schedule
- Tabular display (T x 5) of dividend schedule and metadata (ex date, announced date, pay date, amount, status)
- Input: XML via Solace
- Output: Upload to Sophis

## Correlation Pair / Term Structure / Skew
- Tabular display (T x 3 + expiry) for corr, bear skew, bull skew
- Input: XML via Solace
- Output: Upload to Sophis


## Index Compositions
- Tabular display (N x 3) for symbol, shares, weight with metadata like divisor
- Input: XML via Solace

## Line Pricer
- Price vanilla and light exotic options
- Input: User Input, Nemo Files, Market data (cvi/repo/dividend/corr all xml via solace subscriptions)
- Output: Definition for scenarios, etc

## Vol Slice Viewer
- Input: Vol (KDB, OPRA, BBG), CVI (calc from CVI param data)
* Show Vol Path (Scatterplot)


## Vol Term Structure Viewer
- Input: Vol (KDB, OPRA, BBG), CVI (calc from CVI param data)
* Includes Percentiles
* Events

## Timeseries Viewer
- KDB, Mongo, REST API
* Show density, easy to do spreads, ratios, etc

## Pricer
* Line pricer. Packages (multiple row groups) for mono options. Later extended to multiunderlying.
* Scenario Bumps
* Pull listed prices
* Columns for fwd vol (appropriate for calendars)
* Columns for fwd repo (appropriate for indices with futures even.)

## Instrument Viewer
- Input: XML via Solace


## Trade History
??? SOPHIS?


## Broker Quotes
- List of option structures quoted in the market
* Rest? Websocket? 


## Sales Credit Reports 
* REST or Solace




# Interactions
Blotter -> Launch CVI, Dividend, Repo for the underlying in the tree, if applicable
Blotter -> Launch Slice Viewer, Term Structure Viewer for underlying in tree, if applicable
Blotter -> Launch Trade History for position_ref in tree, if applicable.
Blotter -> Changing rows emits a scope that can be used by other tiles to filter. For example Scenario tiles that are "drilled into" by navigating blotter.
Line Pricer -> Launch CVI, Dividend, Repo for the underlying in line
Line Pricer -> Launch Slice Viewer, Term Structure Viewer for underlying in tree, if applicable
Line Pricer -> Generate output for Scenario Panels
Vol Watchlist -> Launch Timeseries Viewer for underlying and column


## Classifications module


