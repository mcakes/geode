# `geode-tile` Following Query: the Shared Flip-Barrier Helper — Design

Group G, second slice, of the 2026-09-25 codebase review (`architecture.md` M2).
Follows the interaction-doors slice
(`2026-09-27-geode-tile-interaction-doors-design.md`). Approved in conversation
2026-09-27.

## 1. Problem

A scope, grouping or as-of change opens a **flip barrier**
(`crates/geode-shell/src/frame.rs`). Following tiles stage their results until
every participant answers or `FLIP_DEADLINE` (250 ms) passes, then promote together.
This stops one frame from showing tiles evaluated under different global states
(`docs/current/shell.md`). The frame provides only primitives: `open_flip`,
`barrier_wants`, `arrived`, `sweep`, `barrier_open`.

Each following tile rebuilds the same state machine around them. The copies are:

- **Market-data** (`tile.rs`) and **timeseries** (`tile/data.rs`, `tile/mod.rs`)
  are near-identical. Each has `acted`, `query_in_flight`, `staged`, `last_flip`
  and `tag`, and the methods `follows_changed`, `differs_on_followed`, `self_arrive`,
  `arrive`, `arrive_and_release` and `promote`.
- **Blotter** (`tile.rs`) does the same machine inline.
- **Pricer** and **diagnostics** carry an identical ~10-line self-arrive observer.

About thirty mutation entries guard this machine in three places. Hard-won rulings
live in every copy:

- a failed outcome still arrives;
- a stale tag is not an arrival;
- a refusal arrives *before* clearing `acted`;
- promotion compares followed counters, not flip identity;
- `requery` always clears the stage;
- promote on a new flip before the visibility check;
- self-arrive never answers for a same-flip query still in flight.

A fix to one of these lands in one copy.

The copies also disagree on one behaviour, and that disagreement is accidental.
Hiding a market-data or timeseries tile cancels its query and clears `acted`.
Hiding a blotter does neither.

## 2. Rulings (Matthew, 2026-09-27)

1. **Hide and close are different.**
   - **Hiding** a tile (switching tab or workspace) lets its in-flight query finish,
     and the reply applies when it lands. This is the blotter's current behaviour.
     Market-data and timeseries adopt it.
   - **Closing** a tile cancels its in-flight query, in all three.
2. **The helper lives in `geode-tile` and does not depend on `geode-data`.** Every
   refusal path uses `Refusal` only to format its notice. The helper takes
   `submitted: bool`, and the tile formats its own notice.

## 3. The helper

A `following` module in `geode-tile`, with one state type `FollowingQuery<T>`. `T`
is the tile's result: a blotter snapshot plus its grouping, a market-data snapshot,
or a timeseries series result.

**It owns:**

| Field | Holds |
|---|---|
| `acted: Option<FrameVersions>` | the versions the last submit answered |
| in-flight | whether a submitted query has not yet been answered |
| `staged: Option<(T, FrameVersions)>` | a result held for the barrier |
| `last_flip` | the flip generation last seen |
| `tag` | the request generation |

**The tile supplies:**

- its `QueryKey`;
- what it follows, as `differs_on_followed(a, b) -> bool`:
  - blotter: scope unless unscoped, grouping unless pinned, as-of if following, plus
    data and config;
  - market-data: as-of or data;
  - timeseries: as-of;
- how to apply a result.

**Rules, in the order the tile meets them:**

1. **Frame change.** On a new flip, promote first, *before* the visibility check, so
   a hidden tile still promotes. If visible, the helper answers either *requery*
   (something followed changed) or *self-arrive*. Self-arrive is skipped while a
   query with the same flip identity is in flight, because that query's outcome is
   the answer.
2. **Submit.**
   - `begin(versions) -> tag` clears the stage, bumps the tag, records `acted` and
     sets in-flight.
   - The tile then reports `submitted: bool`. On `false` the helper arrives with
     `acted` *first*, then clears `acted` and in-flight, so the next change retries.
3. **Delivery.**
   - A stale tag is dropped without arriving.
   - On `Ok`, if the barrier wants this key, the result is staged. If this arrival
     released the barrier, it is promoted at once. Otherwise it is applied.
   - On `Err`, the tile shows its error and the helper arrives. One broken tile never
     holds the rest open.
4. **Promote.** Take the stage and apply it only if nothing the tile follows moved
   since it was staged. So a stage survives a barrier replaced by a change the tile
   does not follow.
5. **Hide.** Nothing is cancelled and `acted` is kept. A reply that lands while
   hidden applies. Hidden tiles are not barrier participants (`visible_tile_keys`),
   so the barrier never waits on them.
6. **Reshow.** Requery only if something the tile follows moved while it was hidden,
   judged against `acted` or the applied versions. Otherwise nothing.
7. **Close.** Cancel the in-flight query, then arrive at any open barrier that still
   wants this key, so a flip is not held to its deadline by a tile that no longer
   exists. The plan finds the close seam (occupant removal or drop) and wires it
   once for all following tiles.
8. **Non-following tiles.** `arrive_immediately(frame, key, cx)` replaces the pricer
   and diagnostics observers.

**Per-tile options, explicit rather than copied:**

- **Keep `acted` on a non-submit.** The blotter's unresolved named scope arrives but
  keeps `acted`, because a config change is its retry.
- **Post-step hook.** Timeseries runs `release_view` after every promote and every
  delivery, and `release_view` is suppressed while a stage is held.

## 4. Migration

Each step keeps the suite green and is separately reviewable.

1. **The helper**, with pure tests against a real `Frame`.
2. **Market-data**, the closest shape, proves the API.
3. **Timeseries**, including the post-step hook.
4. **Blotter**: the inline copy, the keep-`acted` option, and its
   `unwrap_or_default()` on `acted` removed.
5. **Pricer and diagnostics** move to `arrive_immediately`.
6. **Hide and close**, the only behaviour change:
   - market-data and timeseries stop cancelling on hide, and their clear-on-hide is
     removed;
   - all three cancel and arrive on close;
   - reshow follows rule 6.

**What a trader notices:**

- A market-data or timeseries tile hidden mid-query finishes, and shows its result
  on return, without a requery.
- Closing a tile during a scope, grouping or as-of change no longer holds the other
  tiles to the 250 ms deadline.

Nothing else changes.

## 5. Testing

- **Helper, pure:**
  - a stale tag is not an arrival;
  - a failure arrives;
  - refuse: arrive, then clear;
  - promote-on-flip happens before the visibility gate;
  - a stage survives a replaced barrier that nothing followed moved;
  - a stage is dropped when a followed counter moved;
  - the in-flight self-arrive guard;
  - the keep-`acted` option;
  - the post-step hook;
  - close arrives and cancels.
- **Modules.** Every existing barrier test passes unchanged: about 25 across
  blotter, market-data, timeseries, pricer and diagnostics, all through production
  routes. The tests that pin the old hide behaviour are rewritten in migration step
  6 to pin the new rule, and the commit names them. These include market-data's
  `a_tile_hidden_mid_flight_requeries_on_reshow` and the hide-clears-`acted` checks.
- **New module tests**, one set per following tile:
  - hide mid-flight, reply lands, reshow shows it with no requery;
  - hide, a followed change, reshow requeries;
  - close during an open barrier releases it before the deadline and cancels the
    query.
- **Harness.** Existing entries anchored in barrier code are re-aimed at the line
  that now carries their contract, never deleted. Where several modules' entries now
  guard one helper line, the module-route tests stay the named tests, and the
  checker's REDUNDANT/ALSO rules decide. New entries cover close. Each entry is
  verified three ways: `--build-check`, a named run reporting `caught`, and a hand
  application failing on an assertion.

## 6. Documentation

- `docs/current/shell.md`, the barrier paragraph: the hide and close rules.
- `docs/current/features.md`: the blotter, market-data, timeseries, pricer and
  diagnostics entries wherever they describe hiding or arrival.
- `crates/geode-tile/README.md`: the `following` module, and the note that the
  crate deliberately takes `submitted: bool` rather than depending on
  `geode-data`.
- Module READMEs: the local barrier machinery removed from their module maps.
