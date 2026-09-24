# Geode — Market-Data Egress (documents slice 1, Part 4)

Status: approved design, 2026-09-23. Supersedes §9 of
`2026-09-12-geode-market-data-documents-design.md` where the two differ,
and settles the Part 4 items parked in §6.6 and §9 of
`2026-09-19-geode-dividend-schedule-and-choice-design.md`.

## 1. Rulings (2026-09-23 brainstorm)

1. **No nullable document columns.** `announced_date` and `pay_date`
   are always present on the wire: an undeclared dividend carries
   *estimated* dates in the inbound XML. `DIVIDEND` marks both
   `required: true`, agreeing with `DividendKind`, which already refuses
   a `<dividend>` missing either. `incomplete_rows` then counts exactly
   what an upload refuses.
2. **`dividend_id` is internal.** It is not part of the wire in either
   direction: inbound documents carry no `<id>`, and an upload writes
   none. Geode mints the id at parse time for its own identity (draft
   edits, anchors, rebase, session).
3. **One branch**, built in order: id minting and draft fixes (headless),
   then egress config and request, then the shell delivery, then the
   panel's upload and echo.

## 2. The minted dividend id (`geode-documents`)

`DividendKind::parse` no longer reads `<id>` and `write` no longer
emits one. An `<id>` element in an inbound document is an unknown element
(skipped, reported once per path, as today).

`parse` mints `dividend_id` from content through one public function,
`geode_documents::dividend::mint_ids(ex_dates: &[NaiveDate]) -> Vec<String>`:

- the ex date in `%Y-%m-%d`, for the first row with that ex date in feed
  order;
- `<date>#<n>` for the `n`th (`n ≥ 2`) row with the same ex date, in feed
  order.

So a schedule `[2026-09-18, 2026-12-18, 2026-09-18]` mints
`[2026-09-18, 2026-12-18, 2026-09-18#2]`. The id is stable across
republishes while a row's ex date and its place among same-day rows are
unchanged. The echo check (§7) never reads the id, so the same function
is the only minting door.

A minted id never begins `new-` (it begins with a digit), so it cannot
collide with a draft's own `new-<n>` labels. The parse-time `new-` guard
is removed: there is no wire id left to guard.

**Rebase guard.** An ex date changing upstream makes a row's id vanish,
and `Draft::rebase` already reports a vanished label by name. The
remaining hazard is a same-ex-date group whose membership changes: its
ordinals shift, and an edit keyed `2026-09-18#2` would land on a
different row. `rebase` therefore refuses to carry any cell edit or
`Deleted` mark whose label is in a same-date group (labels sharing the
`<date>` prefix) whose SIZE differs between the draft's base document
and the newer one, and names each refused label — the explicit error
over the plausible wrong value. The size of a group in the base
document is recorded on the draft when the first edit in that group is
made (`Draft.group_sizes`, persisted with the session).

**Known limitation.** A pure reorder of two same-day rows upstream, with
no change of group size, swaps their ids undetectably and a rebased
edit lands on the other row. Recorded in `docs/current/features.md`'s
market-data limitations; the fix is an upstream key, which the XSD may
supply.

The demo generator (`geode_demo_data::documents::dividend`) stops
writing `<id>`, and its "ids stable across republishes" test becomes
"minted ids stable across republishes" through `mint_ids`. Its own id
counter stays internal to the generator's row identity. The demo
database directory is deleted after the change (CLAUDE.md rule).

## 3. Draft fixes (`geode-marketdata`)

- **`set_row_cell` leaves `Sent`.** Writing a cell of an inserted row
  moves `Sent` to `Editing`, as `set`, `set_attr`, `insert_row` and
  `delete_row` already do. Without it a matching echo would clear an
  edit made after the upload.
- **`bump` lands the declared type.** `Draft::bump` takes each cell's
  declared `ColumnType` beside its current value: `F64` lands
  `Value::F64(current + delta)`; `I64` lands `Value::I64` and refuses a
  delta with a fractional part (`bump: <column> takes whole numbers`)
  before writing any cell. No shipped spec has an editable `I64` number
  column today; the rule exists so the first one cannot upload an `F64`.
- **The upload checks every value's tag** against its column's declared
  type while assembling the document (§6) and refuses a mismatch naming
  the row and column. No coercion.
- **`DIVIDEND` flags** per ruling 1.

## 4. Egress config (`geode-core` + `geode-data`)

A new layered config document, `egress.toml`:

```toml
[egress.sophis]
adapter = "demo_bus"                          # a registered adapter name
target = "marketdata/upload"                  # adapter-specific address
documents = ["cvi_params", "dividend_schedule"]
```

Typed reading in `geode-core` (pure, beside `source_config`), keyed by
target name, TOML order preserved. Load-time diagnostics (the target is
dropped, other targets load):

- an adapter name the registry does not know;
- an adapter whose `egress()` answers `None` at resolution;
- a `documents` entry naming no document dataset;
- an empty `documents` list.

Hot reload keeps the last valid set, as every document does. The demo
layer declares `[egress.sophis]` on `demo_bus` for both documents.

## 5. Request, outcome and delivery

```rust
// geode-data
pub struct UploadParams {
    pub key: QueryKey,        // the requesting tile
    pub tag: u64,             // the tile's upload counter
    pub target: String,
    pub document: String,     // document dataset name
    pub rows: DocumentRows,
}
pub struct UploadOutcome {
    pub key: QueryKey,
    pub tag: u64,
    pub target: String,
    pub result: Result<(), String>,
}
```

- `DataHandle::upload(params) -> bool` submits `Request::Upload` through
  the same bounded channel as every request; `false` is a refusal the
  panel reports ("upload refused: data service busy") with the draft
  left `Editing`.
- The service thread resolves the target and the document kind, calls
  `write`, and hands the bytes to that target's `Egress` on a dedicated
  egress thread per target (a slow transport never blocks the service
  loop). The egress thread emits `DataEvent::Upload(UploadOutcome)`.
  A `write` error, an unknown target and an adapter error are all
  `Err(String)` naming the target.
- One upload in flight per target: a second request while one is
  running queues behind it (bounded, capacity 8; past that the outcome
  is an immediate `Err("egress queue full")`).
- `geode-shell` gains `Delivery::Upload(UploadOutcome)` with the
  outcome's fields as plain types (the shell never names `geode-data`),
  keyed by `key` like `Query`. Every `Delivery` match gains an explicit
  arm; no wildcard.

## 6. The panel: `:upload [target]`

- **Eligible targets** are those whose `documents` lists the panel's
  document. None: `:upload` is refused with "no egress target accepts
  <document>". One: `:upload` needs no argument. Several: the argument
  is required and the command line completes target names.
- **Refusals** (a notice, nothing armed): a clean draft; a `Behind`
  draft ("rebase or revert first: an upload must be of a document you
  have seen whole"); `incomplete_rows > 0` ("N rows incomplete"); a
  draft already `Sent` with no further edits ("already sent").
- **The confirm.** `:upload` arms a one-line confirm in the panel's
  command line: `upload 3 cells, 1 row added, 0 removed of SPX to
  sophis? (y/n)`. `y` sends; any other key cancels with "upload
  cancelled". Focus leaving the tile cancels too.
- **Assembly.** The sent document is the base generation's rows with the
  draft applied in painted order: cell and attribute edits written,
  `Deleted` rows removed, `Inserted` rows spliced at their anchors. On a
  `Minted` row axis the label column is omitted from the rows handed to
  the kind (the kind writes no id). On `y` the panel logs one `info` line
  under `geode::ingest` naming key, target and counts, stores the
  assembled rows as `sent`, and submits.
- **Outcome.** `Ok`: the draft enters `Sent` with `sent_at` (displayed
  through `Clock`), the header reads `sent 14:09`. `Err(e)`: the draft
  stays `Editing` and the header shows `upload failed: <e>` until the
  next edit or upload. An outcome whose `tag` is not the tile's latest is
  ignored.

## 7. The echo

While `Sent`, a delivered generation whose source time differs from the
draft's base is compared with `sent`:

- over every column except a `Minted` row axis's label (the id is
  Geode's, not the wire's), row by row in document order;
- exact for `i64`, `utf8` and `date`; within one ULP for `f64`;
- attributes compared the same way.

**Equal:** the draft clears (`revert`), the panel follows the new
generation, and the header reads `sent 14:09, confirmed 14:10` until the
next edit. **Different:** the draft stays `Sent`, the panel keeps
painting the base with the edits, the header reads `echo differs
(N rows)`, and the trader chooses `:rebase` or `:revert`. A differing
echo does NOT move the draft to `Behind`; `:rebase` from `Sent` is
allowed and yields `Editing`. Cell-level highlighting of the differences
is not built.

The `Sent` draft survives the session (`sent` rows are not persisted; a
restored `Sent` draft compares nothing and reads `sent, unconfirmed`
until the trader reverts or rebases). The demo `ChannelAdapter` echoes
an upload back onto its own subscription, so `--demo` runs the whole
loop.

## 8. Tests, harness, docs

Weighting data ≫ shell ≫ modules.

- **`geode-documents`:** `mint_ids` ordinals and stability; parse ignores
  and reports an `<id>`; write emits no `<id>`; parse → write → parse
  equals with ids re-minted; missing `announcedDate`/`payDate` still
  refused.
- **`geode-core`:** each `egress.toml` diagnostic with its path; TOML
  order preserved.
- **`geode-data`:** upload `Ok` through `ChannelAdapter` and the echo
  arriving as a publish; `write` error, unknown target and adapter error
  each an `Err` naming the target; per-target serialisation; the queue
  bound; a refused submission.
- **`geode-marketdata` core:** `set_row_cell` leaves `Sent`; `bump` on
  `I64` lands `I64` and refuses a fractional delta; the type check at
  assembly; assembly order with inserts, deletes and a hidden label;
  the rebase group-size refusal; the echo comparison (equal, one-ULP,
  differing).
- **Panel window tests:** each refusal; the confirm's `y` and cancel;
  focus loss cancels; `Sent` then clear on a matching echo; `Sent` kept
  on a differing echo; a stale `tag` ignored.
- **Shell:** `Delivery::Upload` reaches the addressed tile only.
- **Harness:** one entry per contract above; `--anchors-only` before
  merge.
- **Docs:** `docs/current/data-path.md` (egress), `features.md` (upload,
  echo, the reorder limitation), `configuration.md` (`egress.toml`), the
  crate READMEs of `geode-data`, `geode-documents`, `geode-marketdata`.

## 9. Not decided here

- The real transport's acknowledgement semantics (what `Ok` means on
  Solace) — the vendor shim, written blind later.
- Wire tags, `currency`/`schedule_date` and the status vocabulary (XSD).
- Clearing a value back to empty, and cell-level echo highlighting.

## 10. Amendments (2026-09-24, from the code survey)

1. **Egress addresses are per document, with `{key}`.** `ChannelEgress::upload`
   publishes on the address it is given, and the echo reaches the panel only
   if that address is one the document's source subscribes to. §4's shape
   becomes:

       [egress.sophis]
       adapter = "demo_bus"
       [egress.sophis.documents]
       cvi_params = "marketdata/cvi/{key}"
       dividend_schedule = "marketdata/dividend/{key}"

   `{key}` is replaced by the document key's parts joined with `/`. An address
   without `{key}` is allowed (one fixed address for every key).
2. **`egress.toml` is restart-required, like `sources.toml`.** Nothing reloads
   sources into `geode-data` live; egress joins the same restart list rather
   than inventing a live path. §4's "hot reload keeps the last valid set" is
   withdrawn.
3. **The kind ignores the id axis on write; assembly keeps it.** `DocumentRows`
   for a dividend must still carry the `dividend_id` axis (the kind's
   vocabulary check and `validate` both require it), so §6's "the label column
   is omitted" becomes: assembly writes the painted labels (`new-<n>` included)
   into the axis and `DividendKind::write` never emits them.
4. **Group sizes are captured by the tile, not at first edit.** `Draft::set`
   has no model. The tile calls `Draft::capture_groups(&base_model)` whenever
   the painted model is the draft's base (before `:rebase`, before a policy
   rebase, and before writing the session). A draft restored from an older
   session with no captured groups applies no guard.
5. **`Sent` is not persisted.** The session already persists no draft state;
   a `Sent` draft restores as `Editing` with its edits (the trader may upload
   again). §7's "sent, unconfirmed" is withdrawn.
