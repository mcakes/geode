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
| `dividend` | `DividendKind`: the dividend-schedule document (`marketData/underlying`, `dividends/currency`, `scheduleDate`, one `dividend` per row with `exDate`, `announcedDate`, `payDate`, `amount`, `status`). The wire carries no row id; `mint_ids` mints the `dividend_id` axis from each row's `exDate` at parse (the first row for a date uses that date; further same-date rows append `#2`, `#3`, and so on in feed order), so a row's identity never depends on an upstream id that arrives late, repeats, or is absent. An inbound `<id>` is simply an unrecognised element. |

The parsers walk `quick_xml` events and return columnar `DocumentRows`.
They report ragged CVI slices with both counts and collect paths for skipped
unknown elements. Writers validate the supplied vocabulary and column shapes
before emitting the supported wire format. Parsing and writing preserve
supported document values, not original XML bytes or unknown extensions.

## Commands

```sh
cargo test -p geode-documents
cargo bench -p geode-documents     # document parse and write
```

## Rules this crate pins

- CVI wire tag names remain unverified against the desk's XSD.
  `SLICE_VALUES` in `src/cvi.rs` is the one place to change them.
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
