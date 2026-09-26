# Pricer templates from config and entry-bar completion

Status: approved in conversation 2026-09-26; spec awaiting review.

Builds on the entry bar (`2026-09-26-pricer-entry-bar-design.md`) and the
line-pricer design (`2026-09-19-geode-line-pricer-design.md` §6.3, the
shorthand and its seven templates).

## 1. Scope

Two branches, in order:

- **A. Templates from config.** Package templates become data in a
  layered `pricer_templates` document. The seven built-ins ship in the
  builtin layer; desk and user layers add templates and may redefine a
  built-in.
- **B. Completion in the entry bar.** Suggestions and a hint for the
  token under the caret, driven by the template set from A and by an
  underlying provider the app hands the pricer.

Rulings (user, 2026-09-26):

- Templates are **tables only**: each leg is a weight, a typed strike
  index, a typed expiry index and call or put. No strike arithmetic, no
  market-relative strikes. The config shape leaves room to add strike
  arithmetic later without breaking stored templates.
- Config **may override** a built-in, e.g. a desk `RR` with the opposite
  sign convention.
- Underlying suggestions come from an **explicitly provided list**. Today
  it is a config list; later it becomes the active *watchlist*, which will
  replace the provider's backing in `geode-app` without touching the
  pricer.

Out of scope: alias vocabularies, whole-line formats, strike arithmetic,
delta or %-of-spot strikes, watchlists themselves.

## 2. Branch A: templates from config

### 2.1 The document

`pricer_templates`, layered like `pricer_views` and merged at depth 1, so
an entry in a higher layer replaces the whole entry of that name:

```toml
[CONDOR]
legs = [
  { weight = 1,  strike = 1, kind = "C" },
  { weight = -1, strike = 2, kind = "C" },
  { weight = -1, strike = 3, kind = "C" },
  { weight = 1,  strike = 4, kind = "C" },
]

[CAL]
legs = [
  { weight = 1,  strike = 1, expiry = 2, kind = "C" },
  { weight = -1, strike = 1, expiry = 1, kind = "C" },
]
```

- `strike` and `expiry` are 1-based: `strike = 2` is K2 in
  `K1/K2/...`. `expiry` defaults to `1`.
- `kind` is `"C"` or `"P"`, case-insensitive.
- `weight` is a non-zero integer. The package quantity multiplies it, as
  today.
- The builtin layer ships the seven tables `CS PS STRD STRG RR FLY CAL`
  in this form, as `BUILTIN_TEMPLATES` (the `BUILTIN_VIEWS` pattern). Their
  legs are exactly today's tables.
- `geode-core`'s merge table gains `"pricer_templates" => Some(1)`.

### 2.2 Validation

Each entry is checked on its own. A bad entry is skipped with a
diagnostic under `pricer_templates.<NAME>`, and the rest still load.
Keep-last-valid applies per name. An entry dropped with an error keeps the
previous definition of that name, if there is one, in the entry's own
doc-order position, and adds a warning at the entry's path ("keeping the
previous definition"). "Previous" is the running set on a reload and the
builtin set at startup. A layer's whole-entry replacement therefore
cannot make a name such as `RR` vanish because of a typo. A name absent
from the merged document is removed as normal.

- **Name:** 1 to 8 characters, a letter first, then letters or digits.
  Stored and matched upper-case. `C`, `P` and `CUSTOM` are reserved.
- **Legs:** at least two. Every leg has a non-zero integer `weight`, a
  `strike` of at least 1, an optional `expiry` of at least 1, and a
  `kind`.
- **Indices:** the strike numbers used must be exactly `1..=n` with no
  gap, and the same for expiry numbers. A gap would make the typed
  `K1/K2/K3` ambiguous.
- **Unknown keys** in an entry or a leg are a warning, not an error.
  This is how strike arithmetic can be added later: old binaries warn on
  the new keys instead of refusing the template.

### 2.3 In code

- `Template` stops being an enum. It becomes a `Copy` name handle: an
  upper-case name interned to a `&'static str` (names are few, since they
  come from config and stored sheets, so the interner's leak is bounded).
  `RowKind` stays `Copy`. `Template::CUSTOM` is the distinguished value,
  and the seven built-in names are associated constants.
- A pure `TemplateSet` (`core::template`) maps names to
  `TemplateDef { name, legs: Vec<LegSpec>, strikes, expiries }` and is
  built from the merged document. `LegSpec` keeps its fields, with
  0-based indices internally.
- `shorthand::parse(text, &TemplateSet)` resolves a type token against
  the set, after `C` and `P`. `render_package(&TemplateDef, legs)` is
  unchanged in logic.
- The `Sheet` carries an `Arc<TemplateSet>` (not persisted; the builtin
  set by default), so `Sheet::shorthand(row)` keeps its signature. It
  resolves the package's name and falls back to legs one per line when
  the name is unknown or the legs don't fit its current table.
- The factory holds the current `Arc<TemplateSet>`. Its reload path (the
  one that carries `pricer_views`) replaces it, sets it on every open
  tile's sheet, and rebuilds the model. The app's `PricerConfigKey` gains the merged
  `pricer_templates` value, so an unrelated config edit does not
  rebuild.
- Column 0's tag is the package's stored name, whatever its table
  resolves to. `TREE_WIDTH` is re-derived for the widest allowed name
  (8 characters). The existing width test changes from the widest
  built-in token to an 8-character name.

### 2.4 Stored sheets

- The `template` column keeps storing the name in lower case, as today
  (`cs`), so existing sheets load unchanged. The name is read back case-
  insensitively.
- An unknown name no longer fails the load (today: `unknown template`
  at `storage.rs`). The package loads with that name. It prints its legs
  one per line and shows the name as its tag. Its legs are stored, so
  its prices never change.
- If a desk redefines a template, stored packages of that name whose
  legs no longer fit the new table print their legs one per line and
  keep the tag. Packages whose legs fit the new table print in template
  form.

## 3. Branch B: completion

### 3.1 The pure core

`core::complete` takes the field text, the caret, the `TemplateSet`,
the underlyings and today's date, and returns:

- **the slot at the caret:** `Qty`, `Underlying`, `Expiry`, `Strikes`,
  `Type`, `BarrierKind`, `BarrierLevel` or `None`. The slot is decided
  by token position after an optional leading integer, the way `parse`
  reads it. A caret inside a token is in that token's slot. A caret after
  trailing whitespace is in the next slot.
- **the token's byte range** that a chosen suggestion replaces. For
  `Expiry` and `Strikes` this is the `/`-separated part at the caret, so
  `Z26/H2|` completes `H2`.
- **ranked suggestions:** each a value to write plus an optional detail
  string. Ranking uses `geode_shell::commandline::rank_candidates`, as
  the timeseries expression field does.
- **a hint line** naming the slot and the form expected, e.g.
  `EXPIRY  Z26 · DEC26 · 20DEC26 · 3m`, or for strikes after a known
  type `STRIKES  K1/K2/K3/K4 for CONDOR`.

Suggestions by slot:

| Slot | Suggestions |
|---|---|
| Underlying | the provider's list, in its order before ranking |
| Expiry | the next 8 IMM codes (`Z26`, `H27`, …) and the matching month forms (`DEC26`), from today by the app clock; then `1m 3m 6m 1y` |
| Strikes | none; the hint only |
| Type | `C`, `P`, then every template, the detail being its signature (`4 strikes`, `2 expiries`) |
| BarrierKind | `UI UO DI DO` |
| Qty, BarrierLevel | none; the hint only |

The strikes hint uses the type when the line already has one after the
caret. Otherwise it reads `STRIKES  K or K1/K2/…`.

The core is pure and never formats in render. Its result is rebuilt on
every text or caret change and cached on the tile.

### 3.2 The bar

- The list hangs under the bar over the table: at most 8 rows, scrolling
  with the lit row. The hint line sits between the field row and the
  error line.
- Keys follow the timeseries expression field
  (`docs/current/features.md`, the expression field paragraph).
  - Tab writes the lit suggestion over the token range, and repeated Tab
    cycles.
  - Shift+Tab cycles back.
  - A row click writes that suggestion and keeps the keyboard in the
    field.
  - Each completion is one edit in the field's undo history.
- `enter` keeps its meaning: it adds the line. Unlike the expression
  field, it does not first expand a unique partial match. A shorthand
  token is short, and an auto-expanded underlying would price the wrong
  name silently.
- `up`/`down` keep walking history. The list is never navigated with
  arrows, so history and completion never fight over a key.
- An empty provider shows `no underlyings configured` in the list for
  the underlying slot.

### 3.3 The underlying provider

```rust
pub trait UnderlyingSource {
    /// In the provider's own order (a watchlist's order, later).
    fn underlyings(&self, cx: &App) -> Rc<[SharedString]>;
    /// Bumped whenever the list changes; the tile re-reads on a change.
    fn revision(&self, cx: &App) -> u64;
}
```

- `PricerFactory::new` takes `Rc<dyn UnderlyingSource>`. The tile reads
  it when the bar opens and when `revision` has moved since the last
  read (checked on each text change). There is no subscription and no
  new global.
- `geode-app` backs it today with `[pricer] underlyings = [...]` from the
  app config, upper-cased, deduplicated in order, and refreshed on
  config reload (bumping the revision).
- The trait lives in `geode-pricer`. When watchlists land, the app
  implements it over the active watchlist. If another module needs the
  same seam, it moves to `geode-core` then, not now.

## 4. Tests

Branch A:

- `TemplateSet` from the builtin document equals today's seven tables,
  leg for leg.
- Validation: each rule rejects its entry with a diagnostic path and
  keeps the others. An unknown key warns and still loads.
- Override: a user `RR` with the opposite signs parses `RR` lines to the
  new legs. A stored sheet with old-convention `RR` legs loads, keeps the
  `RR` tag and prints legs one per line.
- A user `CONDOR` parses, renders back in template form, round-trips
  through storage, and is found by `/CONDOR`.
- A stored package whose template name is gone loads (no refusal) with
  its tag and legs.
- Reload: editing `pricer_templates` changes how an open tile parses and
  prints. An unrelated config edit does not rebuild.

Branch B:

- The slot at each caret position across a full line, including after a
  leading qty, inside `/` parts, and after trailing whitespace.
- Expiry suggestions from a fixed date are the next 8 IMM codes and month
  forms, then the tenors.
- Type suggestions include a config template with its signature.
- Through the bar's production routes: typing then Tab writes the lit
  suggestion, Tab cycles, a click writes and keeps focus, `enter` adds
  the line as typed, and `up` walks history with the list open.
- The provider's revision change reaches an open bar.

Mutation entries for the slot rules, the index-gap validation, the
unknown-name load fallback, and Tab's replacement range.

## 5. Docs

`docs/current/features.md` (pricer section: templates, the entry bar's
completion), `docs/current/configuration.md` (the `pricer_templates`
document and `[pricer] underlyings`), and `crates/geode-pricer/README.md`.
