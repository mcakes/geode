# geode-documents

Document kinds: one typed parser and writer per market-data wire format.
Pure, with no I/O and no gpui. `geode-app` registers each kind into the
data service at startup; `geode-data` itself never depends on this crate
and sees only the `geode_core::document::DocumentKind` trait.

Current behavior and rationale:
[`docs/current/features.md`](../../docs/current/features.md#market-data-documents).

## What lives here

| Module | Holds |
|---|---|
| `cvi` | `CviKind`: the CVI parameter document (`marketData/underlying`, `cviParams/anchorDate`, `spotRef`, `nodes/node*`, `slices/slice*` with a `term`, one `forward`/`atm`/`skew` per slice and one `param` per node). |
| `dividend` | `DividendKind`: currency, schedule date, and dividend rows carrying ex date, announced date, pay date, amount, and status. `mint_ids` derives row labels from ex date and same-date order; ids are not carried on the wire. |
| `chain` | `OptionChainKind`: one expiry's option chain, keyed `underlying_ref, expiry` (`marketData/underlying`, `optionChain/expiry`), with `forward`, `spotRef` and `quoteTime` attributes and one `quote` per strike carrying bid, ask and mid vols and bid and ask prices. |

The parsers walk `quick_xml` events and return columnar `DocumentRows`.
They report ragged CVI slices with both counts and collect paths for skipped
unknown elements. Writers validate the supplied vocabulary and column shapes
before emitting the supported wire format. Parsing and writing preserve
supported document values, not original XML bytes or unknown extensions.
Source-startup compatibility and publication validation have separate
responsibilities; see [document validation and storage](../../docs/current/data-path.md#document-validation-and-storage).

## Commands

```sh
cargo test -p geode-documents
cargo bench -p geode-documents     # document parse and write
```

## Rules this crate pins

- CVI and dividend wire tag names remain unverified against the desk's XSD.
  `SLICE_VALUES` in `src/cvi.rs` and `TAGS` in `src/dividend.rs` pair wire
  tags with column names for both parser and writer.
- Option-chain quotes are sorted by strike at parse, and a repeated strike is
  refused naming it, so a chain's strikes are always strictly ascending. Every
  quote must carry all five values: vols arrive computed upstream, so a quote
  missing one is refused rather than filled by averaging bid and ask.
  `quoteTime` must be an RFC 3339 time and is stored as its wire text, since
  document columns cannot be timestamps. The writer refuses anything the
  parser would never produce (strikes not strictly ascending, a non-positive
  strike or forward, an expiry not spelled `YYYY-MM-DD`, a blank or padded
  underlying, a non-RFC 3339 quote time, no rows), so it emits only what the
  parser could have produced. `TAGS` in `src/chain.rs` pairs the quote's wire
  tags with column names; they are unverified against any desk XSD.
- Parse and write failures return typed errors; the data service owns source
  health and diagnostic routing.
- The same kind writes demo and uploaded documents and parses subscribed
  documents, so the in-process bus exercises the normal wire path. XML
  formatting and unknown elements are not retained.
- `mint_ids` is the one door that assigns a dividend its `dividend_id`; a
  minted id is stable only while its ex date and its ordinal among that
  date's rows are unchanged, and never begins `new-` so it cannot collide
  with a draft's own inserted-row labels. A pure upstream reorder of two
  same-day rows, with the group's size unchanged, swaps their ids
  undetectably — `DividendKind::write` never emits an id at all, so this is
  purely a parse-time identity, not a wire contract.
