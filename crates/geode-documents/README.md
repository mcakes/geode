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
| `dividend` | `DividendKind`: the dividend-schedule document (`marketData/underlying`, `dividends/currency`, `scheduleDate`, one `dividend` per row with `exDate`, `announcedDate`, `payDate`, `amount`, `status`). The wire carries no row id; `mint_ids` mints the `dividend_id` axis from each row's `exDate` at parse (`#n` for the `n`th row sharing a date, feed order), so a row's identity never depends on an upstream id that arrives late, repeats, or is absent. An inbound `<id>` is simply an unrecognised element. |

The parser is a hand-written `quick_xml` event walk rather than a serde
derive for three reasons: the ragged-slice rule needs
both counts in the error, the unknown-element rule needs the path of the
element that was skipped, and the parse lands straight in struct-of-arrays
`DocumentRows` with nothing allocated per row. The file is expected to be
regenerated from the desk's XSD later, behind the same two functions
(roadmap ruling 8). The shape of the seam is what matters.

## Commands

```sh
cargo test -p geode-documents
cargo bench -p geode-documents     # document parse and write
```

## Rules this crate pins

- CVI wire tag names are an assumption until the desk's XSD arrives.
  `SLICE_VALUES` in `src/cvi.rs` is the one place to change them.
- A parse failure is reported per `(source, path)` and never panics.
- A kind produces `DocumentRows` and writes them back byte for byte
  through the same trait, which is what lets the demo bus in `geode-app`
  exercise the real subscribed-source path with no broker.
- `mint_ids` is the one door that assigns a dividend its `dividend_id`; a
  minted id is stable only while its ex date and its ordinal among that
  date's rows are unchanged, and never begins `new-` so it cannot collide
  with a draft's own inserted-row labels. A pure upstream reorder of two
  same-day rows, with the group's size unchanged, swaps their ids
  undetectably — `DividendKind::write` never emits an id at all, so this is
  purely a parse-time identity, not a wire contract.
