# Geode — cross-cutting architecture and philosophy conformance review

Scope: the whole workspace, read-only. Passes 1–9 complete; pass 10 (next-phase
levers) complete as an assessment of readiness and cheapest seam, not a plan.
No build, test, clippy or bench was run. Every finding below cites code I read.

## (a) Summary

1. **The declared dependency rules hold, exactly.** `geode-core` has no gpui,
   no DuckDB and no socket; `geode-shell` and `geode-data` never see each other;
   no feature crate depends on a sibling; `geode-pricing`, `geode-chart`,
   `geode-widgets` and `geode-documents` are leaves; `geode-app` is the only
   composition root. Every `[lib]`/`[[bin]]`/`[[example]]` sets `bench = false`
   and every `[[bench]]` sets `harness = false`. This is unusually clean for a
   14-crate workspace written in a month.
2. **The coupling moved from `Cargo.toml` into the module contract.**
   `TileContent` has 10 required methods and provides nothing. So each of five
   modules independently re-implements the popup surface, the `.` action menu,
   the notice slot, flip-barrier staging, the refusal message, the stale clock
   and the vim motion keymap. Philosophy §4's "a module never invents its own
   navigation idiom" is being honoured by five parallel careful copies rather
   than by a shared mechanism.
3. **Philosophy §1 (lens, not brain) holds strictly.** No crate but
   `geode-pricing` performs financial arithmetic; `geode-pricer` does not even
   name it in `Cargo.toml`. The only arithmetic in a module is a package row
   summing its legs (explicitly permitted) and percentiles pushed into SQL.
4. **Philosophy §3 (silence is a bug) holds at every refusal site I traced**,
   and honestly — each refusal names the door and clears `acted` so the barrier
   cannot hang. Two gaps: `DataHandle::dropped_requests()` has no reader in the
   workspace, and the log ring cannot notify, so the log tail repaints only on
   unrelated activity.
5. **The structural debts that will bite next** are `geode-shell` at 93.7k lines
   (40% of the workspace) with a 72-field `ShellView` and nine parallel dialog
   slots, `marketdata/tile.rs` at 14,729 lines, whole-document row-object
   materialisation against the §6 columnar claim, and a `Request`/`DataEvent`
   seam with no serialisation — which is precisely what the ingest-daemon TODO
   requires.

---

## (b) The dependency graph and the `TileContent` implementor table

### Actual crate graph (from every `Cargo.toml`, confirmed with `cargo metadata --no-deps`)

Normal dependencies only, `geode-*` edges (third-party omitted except where a
rule depends on it):

```
geode-app ──► geode-shell, geode-data, geode-core, geode-pricing,
              geode-blotter, geode-marketdata, geode-timeseries,
              geode-pricer, geode-diagnostics, geode-documents,
              geode-demo-data          [+ gpui, gpui-component, gpui-base,
                                          gpui-kit-assets, tracing*, toml]

geode-blotter    ──► geode-core, geode-shell, geode-data
geode-marketdata ──► geode-core, geode-shell, geode-data, geode-widgets
geode-timeseries ──► geode-core, geode-shell, geode-data, geode-widgets,
                     geode-chart
geode-pricer     ──► geode-core, geode-shell, geode-data
geode-diagnostics──► geode-core, geode-shell

geode-shell      ──► geode-core, geode-widgets       [+ gpui, gpui-component,
                                                        gpui-kit-assets]
geode-data       ──► geode-core                      [+ duckdb (bundled)]
geode-widgets    ──► geode-core                      [+ gpui, gpui-component]
geode-chart      ──► geode-core                      [+ gpui, gpui-component]
geode-documents  ──► geode-core                      [+ quick-xml]
geode-pricing    ──► geode-core                      [nothing else at all]
geode-demo-data  ──► geode-core
geode-core       ──► (no geode crate)                [toml, arrow, chrono,
                                                        regex, toml_edit,
                                                        tracing*]
```

Dev-dependency edges that cross the normal graph (test-only, legitimate but
worth knowing): `geode-data` dev-deps `geode-demo-data` (documented in that
crate's README as the reason nothing there may depend on `geode-data`), and
`geode-chart` dev-deps `geode-shell` so its bundled-theme contrast sweep can
call `geode_shell::theme::load_bundled()` (`crates/geode-chart/src/core/palette.rs:80`,
inside `#[cfg(test)]`). The "leaf" claim is therefore a normal-deps claim.

**Rule-by-rule verdict**

| Rule (CLAUDE.md / architecture.md) | Verdict | Evidence |
|---|---|---|
| `geode-core` shared vocabulary, no socket/DB/window | **Holds.** Only `toml`, `arrow`, `chrono(-tz)`, `regex`, `toml_edit`, `tracing`. No gpui, no duckdb. | `crates/geode-core/Cargo.toml:18-30` |
| `geode-core` typed interpretation I/O-free; only `read_docs`/`load` read files | **Holds** as documented; `config/` is the only I/O. | `crates/geode-core/README.md:14-20` |
| shell and data never depend on each other | **Holds.** | `crates/geode-shell/Cargo.toml`, `crates/geode-data/Cargo.toml` |
| features never depend on siblings | **Holds.** The only feature→feature-looking edge is `geode-timeseries → geode-chart`, and `geode-chart` is a presentation leaf below features, not a feature. | `crates/geode-timeseries/Cargo.toml:16` |
| calculation crates are leaves reached by request/outcome | **Holds strictly.** `geode-pricing` depends on `geode-core` alone and on nothing else in the universe; no module or the shell or the data crate names it. `geode-data` mentions the string only as a thread name. | `crates/geode-pricing/Cargo.toml:11-12`; `crates/geode-data/src/pricing/worker.rs:46`; `crates/geode-pricer/Cargo.toml:16` (comment: "Never `geode-pricing`") |
| presentation crates are leaves | **Holds** for normal deps (`geode-chart`, `geode-widgets` → core + gpui only). | metadata |
| `geode-app` is the only composition root | **Holds.** It is the only crate depending on `geode-documents`, `geode-pricing`, `geode-demo-data` and every feature. | `crates/geode-app/Cargo.toml:21-33` |
| gpui pinning consistency | **Holds.** All six gpui/gpui-kit crates are `=`-pinned in one place and every crate uses `.workspace = true`. | root `Cargo.toml:41-46` |
| every target `bench = false`; Criterion `harness = false` | **Holds, all 14 crates.** Every `[lib]`, the one `[[bin]]`, both `[[example]]`s carry `bench = false`; all 17 `[[bench]]`es carry `harness = false`. | every `crates/*/Cargo.toml` |
| test-feature parity | **One break:** `geode-pricing` has no `[dev-dependencies]` at all, so it does not enable `geode-core/test-support`. See Minor 15. | `crates/geode-pricing/Cargo.toml` |
| a feature reaches data only through `DataHandle` | **Holds.** Every `geode_data::` reference from a module is `DataHandle` or a `*Params` struct passed to it — no store, query, ingest or adapter type is named. | grep of all five feature crates |
| shell types leak no data concepts | **Holds.** `Delivery` carries only `geode_core` outcome types (`QueryOutcome`, `PriceOutcome`, `SeriesOutcome`) and a plain `UploadDelivery`. | `crates/geode-shell/src/module.rs:21-38` |
| arrow named nowhere outside `snapshot.rs` | **Almost.** `geode-data` names `duckdb::arrow::…` twice, which is DuckDB's re-export rather than the `arrow` crate, and both are at the DB boundary. | `crates/geode-data/src/query/pool.rs:439`, `crates/geode-data/src/ingest/load.rs:1068-1073` |

**Near-violations worth naming:** `geode-marketdata`, `geode-pricer` and
`geode-timeseries` reach into `geode_shell::shell::*` internals —
`shell::dialog::init_reclaimed_keybindings` (3 call sites), `shell::chip`,
`shell::listrow::row_paint`, `shell::control`, `shell::colours`, `shell::scale`,
and one module reads `shell::ShellServices` and `shell::toolbar`. These are
public but they are the shell's *interior* module path, not a curated module
API. `geode-shell/src/lib.rs` exports 33 `pub mod`s, so there is no narrow
"what a module may use" surface at all.

### `TileContent` implementor table

The trait (`crates/geode-shell/src/module.rs:186-289`): 10 required methods,
`holds_focus` defaulted, `stack_handle_for_test` defaulted under
`cfg(test, feature="test-support")`. No provided behaviour of any kind.

| Trait method | blotter | marketdata | timeseries | pricer | diagnostics | placeholder |
|---|---|---|---|---|---|---|
| `key_context` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `dispatch` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `command` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ (refuses) |
| `completions` | ✓ | ✓ | ✓ | ✓ | ✓ (pure fn) | ✓ |
| `find` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `deliver` | Query only | Query + Upload | Series + SeriesFetched | Price only | **all arms empty** | all empty |
| `set_visible` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `set_stack` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `title` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `serialize` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ (record passthrough) |
| `holds_focus` | default (false) | ✓ | ✓ | ✓ | default | default |

Sources: `geode-blotter/src/content.rs:102-146`,
`geode-marketdata/src/content.rs:184-233`,
`geode-timeseries/src/content.rs:147-215`,
`geode-pricer/src/content.rs:212-280`,
`geode-diagnostics/src/lib.rs:76-120`,
`geode-shell/src/module.rs:480-520`.

### What each module hand-rolls beyond the contract

| Mechanism | blotter | marketdata | timeseries | pricer | diagnostics | shell has one? |
|---|---|---|---|---|---|---|
| Vim motion keymap (`j k gg G ctrl+d/u/f/b`) | own action ids | own | partial | own | own | `vimnav`/`listfilter` cores exist; the **bindings** are per module |
| `:` vocabulary + completions | ✓ | ✓ | ✓ | ✓ | ✓ | `commandline` parses the line only |
| Insert mode / `InputState` ownership | — | 34 refs | 13 | 13 | — | no shared editor-in-tile facility |
| Action menu (`.`) | — | `core/menu.rs` `MenuRow` | `core/menu.rs` `MenuRow` | `popup.rs` `MenuItem` | — | **no** — three near-identical types |
| Anchored popup surface | — | `popup.rs:40` | `popup.rs:551` | `popup.rs:28` | — | **no** — `popover_surface` is byte-identical in all three |
| Typeahead / choice list | — | `ChoiceList` 16 | 9 | 6 | — | `choice` core exists; the popup does not |
| Confirm prompt (y/n) | — | 88 refs (`arm_upload`) | — | 1 | — | `dialog.rs` has a modal confirm; the in-tile one is marketdata's own |
| Notice slot | `error: Option<String>` | `notice: Option<SharedString>` + `upload_error` | `notice` | `notice` + `view_notice` + `save_notice` | — | `ShellView.notice` carries exactly **one** message |
| Header/toolbar | own | `header.rs` | `header.rs` | `header.rs` | own | shared chip/listrow/control **paint** doors only |
| Freshness / `stale_after` | `Rc<Cell<Duration>>` | `Duration` | — | `PricerSettings` | — | no shared staleness display |
| Help line / footer | 10 refs | — | 11 | 69 | — | `footer` core exists, used unevenly |
| Session persist/restore | ✓ | ✓ | ✓ | ✓ | ✓ | opaque `toml::Table`, no schema help |
| Submission + refusal handling | ✓ | ✓ | ✓ | ✓ + backoff | — | **no** — four independent copies |
| Flip-barrier staging | `acted`/`staged`/`follows_changed`/`promote` | same six methods | same six methods | self-arrive | self-arrive | `Frame::{open_flip,barrier_wants,arrived}` primitives only |
| Delivery routing | 1 arm + 4 empty | 2 + 3 empty | 2 + 3 empty | 1 + 4 empty | 0 + 5 empty | exhaustive enum (good) |

---

## (c) Findings

### Critical

**None.** I looked for one and did not find one at this layer. The dependency
rules hold in the build graph, the calculation leaf is genuinely unreachable
from a module, `Delivery` is exhaustive so no outcome can be dropped silently
at compile time, and every refusal path I traced produces a visible message and
clears the barrier. The debts below are design debts and budget risks, not
defects that produce a wrong number. Crate-level reviewers are better placed to
find a Critical inside one subsystem.

### Major

**M1. `TileContent` supplies no behaviour, so five modules each re-implement the same seven mechanisms.**
`crates/geode-shell/src/module.rs:186-289` (the trait: 10 required methods, one
defaulted, zero provided). The table above enumerates the duplication. The
sharpest instances: `popover_surface` is the same nine lines in
`geode-marketdata/src/popup.rs:40-47`, `geode-pricer/src/popup.rs:28-36` and
`geode-timeseries/src/popup.rs:551-559`; three `MenuRow`/`MenuItem` enums with
the same `Action { id, title, hint, enabled, checked } | Separator | Section`
shape at `geode-marketdata/src/core/menu.rs:25-40`,
`geode-timeseries/src/core/menu.rs:21-36` and
`geode-pricer/src/popup.rs:129-148`; four independent "refused: the data service
is busy or gone" strings at `geode-blotter/src/tile.rs:765`,
`geode-marketdata/src/tile.rs:1210`, `geode-timeseries/src/tile/data.rs:147,197`,
`geode-pricer/src/tile.rs:72`. Philosophy §4 says a module must not invent its
own navigation idiom; today it must, because the shell offers a paint door
(`chip`, `listrow`, `control`) but no interaction door. Every one of these copies
is *correct* — each carries the same hard-won rulings (`occlude()`, blur before
drop, a disabled row takes no fill) — which is exactly the problem: the sixth
module will re-derive them, and a fix lands in one copy.
**Impact:** the cost of a new module is a rewrite of the interaction model, and
a ruling fixed once is fixed in one of three places. This is the workspace's
largest architectural lever.
**Direction:** extract a `geode-tile` crate (below `features`, above `shell`'s
pure cores, or a `geode-shell::tile` submodule) owning: `popover_surface` +
`anchor_popup`, one `MenuRow`/`Menu` with `step`/`snap`/`pickable`, a `Notice`
type with one presentation, a `Refusal` helper that formats and clears `acted`,
and a `TileQuery<T>` that owns `acted`/`staged`/`tag`/`in_flight` and the six
barrier methods. Migrate one module (pricer, the newest) to prove the seam.

**M2. Flip-barrier staging is copy-pasted, in full, into three tiles.**
`geode-shell/src/frame.rs:601-667` provides only the primitives
(`open_flip`, `barrier_wants`, `arrived`, `barrier_open`). Each following tile
then re-derives the same six-member state machine:
`geode-blotter/src/tile.rs:177,215,585,672` (`acted`, `staged`,
`follows_changed`, `promote`); `geode-marketdata/src/tile.rs:529,640,1102,1128,
1152,1160,1993` (`acted`, `staged`, `follows_changed`, `self_arrive`, `arrive`,
`arrive_and_release`, `promote`); `geode-timeseries/src/tile/data.rs:220,243,
265,273,291` (the same five names). Blotter and marketdata carry near-identical
comments recording the *same* review finding at both sites ("A refused submit
means nothing is ever coming for these versions … market-data Part 3 Task 6
review, MIN-3, fixed at both sites under the mechanism rule",
`geode-blotter/src/tile.rs:766-775`). A mechanism ruling being applied by hand at
each site is the documented failure mode in this repo's own memory.
**Impact:** the barrier is the mechanism that prevents a frame showing tiles
evaluated under different global state — a correctness-of-display property — and
it is maintained in three places plus two self-arrive shortcuts.
**Direction:** promote the pattern into the shell as a `FollowingTile` helper
holding `acted`/`staged`/`tag` with `submit_or_refuse()`, `on_outcome()` and
`promote()`; leave `Frame` the pure value it is.

**M3. Whole-document and whole-sheet row-object materialisation, against §6.**
Philosophy §6 requires "columnar data flowing end-to-end without materializing
into row objects". The columnar chain is real from DuckDB to the module
(`geode-core/src/snapshot.rs:387-398`, one `RecordBatch` + column meta), and
then two modules materialise everything:
`geode-marketdata/src/core/matrix.rs:107-111,149` — `MatrixModel.rows:
Vec<RowModel>`, each `RowModel { label: SharedString, cells: Vec<Cell>, state }`,
i.e. two heap allocations per row for the **entire** document;
`geode-pricer/src/grid.rs:51-67` — `GridModel.rows: Vec<GridRow>`, each with
`tree: SharedString` and `cells: Vec<GridCell>` carrying a `SharedString` per
cell, for the entire sheet. The marketdata README concedes the consequence
("The flat build is a per-delivery and per-commit cost at the edge of the 8 ms
budget for a 10,000-row schedule; such a panel must patch cells rather than
rebuild"). Contrast the blotter, which does the right thing:
`geode-blotter/src/core/cache.rs:22-60` keeps only the visible window and reuses
the inner `Vec`s across window moves via `std::mem::take`.
**Impact:** the 8 ms budget is structurally at risk for any document or sheet
larger than a screen, and the mitigation (patch, don't rebuild) is a
per-operation discipline rather than a property of the data structure.
**Direction:** the blotter's windowed `FormatCache` is the pattern the other two
should share — lift it into the shared tile crate of M1 and make the prepared
model struct-of-arrays (`labels: Vec<SharedString>`, `cells: Vec<Cell>` with a
stride) so a row is a slice, not an object.

**M4. A market-data panel costs a release, not a config file.**
`geode-marketdata/src/core/spec.rs:124-150` — `PanelSpec` is entirely
`&'static str` / `&'static [ValueColumn]`, with `pub const CVI` at line 217 and
`pub const DIVIDEND` at line 329. Adding a third document panel therefore means
writing Rust, registering a factory in `geode-app`, and shipping a binary.
Philosophy §5 says the opposite in as many words: "A new view of existing data
should cost a config file, not a release", and the blotter honours it — any
`views.toml` entry becomes a tile. The `&'static` choice also forecloses the
config route without a type change.
**Impact:** the desk cannot add a market-data view; §5's "what one trader
builds, the desk can inherit" does not apply to half the feature surface.
**Direction:** make `PanelSpec` owned (`String`/`Vec`), read it from a
`panels.toml` document with a typed reader beside the existing ones, and keep
`CVI`/`DIVIDEND` as builtin-layer documents rather than consts. The typed-reader
and provenance machinery in `geode-core::config` already does everything needed.

**M5. The request-refusal counter has no reader in the workspace.**
`geode-data/src/handle.rs:80,90,95,220,233` increments `dropped` on every
refused submission and `handle.rs:240-242` exposes
`pub fn dropped_requests(&self) -> u64`. Grep of the whole workspace: the only
callers are that crate's own tests (`handle.rs:423,530,732`). The
diagnostics "dropped events" row is a *different* counter — the event **sink**'s
refusals, `geode-app/src/bridge.rs:277-286` → `bridge.rs:711-722` →
`Diagnostics::note_dropped` → `geode-diagnostics/src/sections.rs:458-464`.
So an individual refusal is visible (each tile paints a notice), but the refusal
*rate* — the signal that says the 64-entry queue is chronically full and the
desk is losing requests — is invisible.
**Impact:** the one aggregate number that would diagnose a saturated data
service is computed and thrown away. Philosophy §3.
**Direction:** add `dropped_requests` beside `dropped events` in the perf
section; it is one row and the value is already an `Arc<AtomicU64>` the bridge
can read on the same tick it reads the sink counter.

**M6. `geode-shell` is 40% of the workspace, and `ShellView` is a 72-field god object.**
93,745 of 233,252 workspace lines. `ShellView`
(`geode-shell/src/shell/mod.rs:~470-600`) has 72 fields, including **nine
parallel dialog states** (`modal`, `keybindings`, `settings`, `picker`,
`as_of_dialog`, `scope_expr_dialog`, `choice_dialog`, `object_dialog`,
`stack_list`) plus five `ScrollHandle`s, three `InputState`s, three
config-baseline vectors and three `scratch_*` collections. `shell.md` says
"`ShellView` … must not become a second domain model", and it has not become a
*domain* model — but it has become the single owner of every transient surface.
Supporting numbers: `shell/objectdialog/mod.rs` 6,189 lines,
`shell/objectdialog/render.rs` 4,755, `tiling/workspaces.rs` 3,234,
`tiling/tree.rs` 2,790, `session.rs` 2,400, `shell/mod.rs` 2,005.
**Impact:** every dialog change touches the same struct and the same render
function; the nine parallel `Option`s are also the direct blocker for the dialog
stack the TODO wants (I9).
**Direction:** one `surface: Option<Surface>` enum (or a `Vec` for the stack)
whose variants own their state and scroll handle. That is a mechanical change
with a large payoff and it makes I9 nearly free.

**M7. `crates/geode-marketdata/src/tile.rs` is 14,729 lines.**
4,646 lines of implementation and 10,083 of tests in one file (first
`#[cfg(test)]` at line 4,647). The next largest implementation files are
`geode-shell/src/shell/objectdialog/mod.rs` (6,189, tests from 3,217),
`geode-data/src/service.rs` (5,501, tests from 405),
`geode-pricer/src/tile.rs` (5,236, tests from 2,426),
`geode-blotter/src/tile.rs` (5,044, tests from 1,400),
`geode-app/src/bridge.rs` (3,554, tests from 934). The coding guides say to
"split a file when state ownership or lifecycle can no longer be understood
without reading unrelated behavior"; a single file owning the cursor, the
editor, the draft, the parked drafts, the upload confirm, the echo comparison,
the popup and the underlying picker is past that line. This repo's own memory
records that a 10k-line file stalls an agent mid-edit.
**Impact:** review surface, merge conflicts, and a practical ceiling on who can
change the panel.
**Direction:** split by lifecycle, not by role — `tile/upload.rs`,
`tile/editor.rs`, `tile/draftstate.rs`, `tile/tests/` — following the
`geode-timeseries/src/tile/{mod,data,popups,pointer}.rs` split that already
exists in this workspace and reads well.

**M8. Every module re-declares the vim motion keymap under its own action namespace.**
`geode-blotter/src/content.rs:46`, `geode-marketdata/src/content.rs:122`,
`geode-timeseries/src/content.rs:74`, `geode-pricer/src/content.rs:101`,
`geode-diagnostics/src/lib.rs:52`. Four of the five bind the identical set
`j k g\ g shift+g ctrl+d ctrl+u ctrl+f ctrl+b h l space escape` to
`<module>::down`, `<module>::up`, `<module>::top`, … Philosophy §2 says
"Keybindings are stable, mnemonic, and user-remappable" and §4 says a module
never invents its own navigation idiom — but a trader who remaps `j` must do it
five times, in five keymap sections, against five action ids. The TODO's "Shared
key bindings" is this.
**Impact:** the muscle-memory promise is per-module; a remap is five edits, and
a sixth module can silently disagree about `ctrl+f`.
**Direction:** a `tile-list` (or `vimnav`) key context the shell contributes
beneath the module's own, with shell-owned actions (`list::down`, `list::top`)
that `TileContent::dispatch` receives. Modules keep their own verbs and lose the
motion boilerplate; one rebind covers every tile.

**M9. No `#[non_exhaustive]` anywhere, and public fields are the default across crate seams.**
Zero occurrences of `non_exhaustive` in 233k lines. Public-field counts at one
indent level: `geode-core` 257 across 78 public structs, `geode-shell` 301
across 99, `geode-data` 196 across 61. These are crate seams:
`ShellServices` (`shell/mod.rs`) is ~18 public fields consumed by `geode-app`;
`DataServiceConfig` is 10 public fields; `QueryOutcome`
(`geode-core/src/query.rs`) is all-public; `MatrixModel` and `GridModel` expose
every field to their own delegates. The coding guides are explicit: "Every public
struct with public fields must carry `#[non_exhaustive]`" and "Private fields
are the default for behavioral state that must evolve".
**Impact:** low today (one binary, no external consumer), rising: any field
addition to a record-like config struct is a breaking change across the graph,
and `MatrixModel`-style all-public models let a delegate mutate prepared state.
**Direction:** apply it where the guide's own carve-out does not (behavioural
state), starting with `ShellServices`, `DataServiceConfig` and the prepared
models; leave genuinely record-like schema/config/geometry types alone but add
`#[non_exhaustive]` to them.

**M10. Errors at every UI seam are `String`.**
~170 `Result<_, String>` signatures: `geode-shell` 52, `geode-marketdata` 32,
`geode-pricer` 30, `geode-timeseries` 28, `geode-core` 11, `geode-data` 8. Only
nine structured error types exist in the whole workspace, all in core and data
(`ClockError`, `EditError`, `LoadError`, `SentinelError`, `StoreError`,
`AdapterError`, four `ParseError`s, `WriteError`, `PricingError(pub String)`).
`Delivery` and `QueryOutcome` themselves carry `Result<Arc<Snapshot>, String>`
(`geode-core/src/query.rs`). No `anyhow` and no `thiserror` anywhere.
**Impact:** a caller cannot branch on a failure kind — "is this queue pressure
or a compile error?" is answered by substring. It also means every user-visible
message is authored at its throw site, which is why the four "busy or gone"
strings in M1 drifted into four literals. Retry policy (pricer's backoff) had to
be built on a boolean rather than on a typed refusal.
**Direction:** do not boil the ocean. Introduce one enum at the one seam that
needs branching — a `Refusal { QueueFull, ServiceGone, Rejected(String) }` on
submission, and a `QueryError` kind on `QueryOutcome` — and let the shared
notice helper of M1 own the wording.

### Minor

**m11. The scope chip's `×` copies a mutation the keyboard reaches by a different path.**
`geode-shell/src/shell/render.rs:1039-1047` calls `Frame::drop_dimension`
directly from the pointer handler; `geode-shell/src/frame.rs:310` shows that
handler is its **only** non-test caller. The keyboard route to the same outcome
is `mod+p` → pick the column → `ctrl+x` → `enter`, which runs
`PickerState::apply` (`shell/picker.rs:171-189`, empty tick set drops the
dimension) → `Frame::set_scope` (`picker.rs:420-428`). So the command exists
twice, by two mutations, and only one of them is named in the registry
(`defaults.rs` has `frame::scope_clear` but no per-dimension drop). `shell.md`:
"One logical command has one action or owner method … pointer handlers route to
that command instead of copying its mutation."
**Impact:** small but exactly the class of drift the rule exists to stop; the
chip's tooltip ("Remove *column*", `scopebar.rs:82`) names no key, so the
keyboard route is also undiscoverable.
**Direction:** register `frame::drop_dimension` taking the focused chip (or the
picker's column), bind nothing by default, and have both the `×` and the
picker's empty-tick commit dispatch it.

**m12. The log ring cannot notify, so the log section repaints only on unrelated activity.**
`geode-core` has no gpui dependency, so `Ring::push`
(`geode-core/src/log/mod.rs:75`) physically cannot notify a GPUI entity. The
diagnostics tile registers exactly three observers —
`cx.observe(&diagnostics)` (`geode-diagnostics/src/tile.rs:139`),
`cx.observe_global::<AppClock>()` (line 158) and `cx.observe(&frame)` (line 163)
— none of which is the ring. The crate's own tests say so out loud: "Ring pushes
carry no notify of their own; `note_dropped` + `cx.notify()` is the existing
tests' stand-in for the real caller … that actually triggers the next rebuild"
(`tile.rs:1008-1012`). In practice health, polls and the 500 ms reload tick keep
it roughly live.
**Impact:** a burst of records during an otherwise quiet moment can sit
unpainted, and "the log follows new records" (features.md) overstates the
guarantee. §3 ("silence is a bug") in the mildest form.
**Direction:** have the shell's existing 500 ms watcher compare
`Ring::latest_seq()` and notify `Diagnostics` when it moved — one comparison on
a tick that already runs.

**m13. `DataHandle::cancel`'s refusal is discarded at all five call sites.**
`geode-data/src/handle.rs:146-148` returns `bool` and increments `dropped` on
refusal, per `send` (`handle.rs:84-99`). Callers:
`geode-marketdata/src/tile.rs:2019`, `geode-timeseries/src/tile/mod.rs:474`,
`geode-pricer/src/tile.rs:444,573,2595`. At
`geode-pricer/src/tile.rs:568-577` the hide path cancels, then clears
`in_flight` and drops both the refresh and retry tasks — on the assumption the
cancel landed. `request-delivery.md` says cancellation "is itself an ordinary
queued request and can be refused" and "has no acknowledgement", and that
receivers still need stale checks (they have them), so the consequence is a
wasted round trip rather than a wrong number.
**Impact:** benign today and documented; it is nonetheless the only place in the
codebase where a refusal is neither surfaced nor acted on.
**Direction:** leave the behaviour, but bind the result (`let _ = …` with a
comment, or count cancel refusals separately) so the discard is deliberate at
each site rather than invisible.

**m14. `geode-chart` duplicates three shell values with a comment instead of a test.**
`geode-chart/src/core/mod.rs:13-16` duplicates `DESIGN_REM = 12.0` because "this
crate must not depend on the shell"; `geode-shell/src/shell/scale.rs:37` is the
original and both are currently `12.0`.
`geode-chart/src/core/palette.rs:17,28` likewise mirror
`geode_shell::shell::colours::{to_rgb,to_hsla}`. The boundary reason is right,
but `geode-chart` already dev-depends on `geode-shell` and calls
`geode_shell::theme::load_bundled()` in a test (`palette.rs:80`), so an
equality assertion is free.
**Impact:** a silent geometry drift if the rem scale ever changes; the shell
test at `scale.rs:59` pins the shell side only.
**Direction:** one `#[test]` in `geode-chart` asserting
`DESIGN_REM == geode_shell::shell::scale::DESIGN_REM`, and the same for the two
colour conversions.

**m15. `geode-pricing` has no dev-dependencies, breaking test-feature parity.**
`crates/geode-pricing/Cargo.toml` has `[dependencies] geode-core.workspace =
true` and nothing else; `src/lib.rs:135` has a `#[cfg(test)]` module. Every
other crate dev-deps `geode-core = { workspace = true, features =
["test-support"] }`, and `geode-core` dev-deps itself with the feature so
`cargo test -p geode-core` and `cargo test --workspace` build one artifact
(README, "Features"). Because `geode-pricing` does not, `cargo test -p
geode-pricing` resolves `geode-core` **without** `test-support`, a different
feature set from every other target in the workspace.
**Impact:** a full `geode-core` rebuild whenever you alternate between that
crate and anything else — the exact hazard this repo fixed once already (memory:
`cccd2fb`, "a new crate's dev-deps must match the workspace's geode-*
features").
**Direction:** add `[dev-dependencies] geode-core = { workspace = true,
features = ["test-support"] }`.

**m16. Logging targets are inconsistent, and an untargeted record is not level-controllable.**
`geode-core/src/log/mod.rs:14-20` declares seven `geode::*` targets, and
`LogLevels::to_targets` (`log/mod.rs:327-335`) builds
`Targets::new().with_default(WARN).with_target("geode", default)` plus one entry
per configured suffix — so a record emitted with **no** target gets its crate
path as the target (e.g. `geode_marketdata`), which matches neither `geode` nor
any suffix, and is therefore capped at `WARN` and invisible to `[log]`
(pinned by that module's own test, `log/mod.rs:534-553`). Untargeted call sites
in non-test code: `geode-data` 21, `geode-shell` 14, `geode-app` 6,
`geode-marketdata` 4, `geode-pricer` 4, `geode-blotter` 2, `geode-core` 1 — 52
in all, against 61 targeted. Four crates (`geode-chart`, `geode-documents`,
`geode-timeseries`, `geode-widgets`) emit nothing at all, and
`geode-timeseries` has no `tracing` dependency, so its refusals reach only the
tile notice. Examples: `geode-marketdata/src/tile.rs:1308,1324,1343,1559`
(upload lifecycle, untargeted), `geode-pricer/src/tile.rs:670` (a rollback
failure at `error!`, untargeted), `geode-data/src/pricing/worker.rs:152-227`
(seven untargeted).
**Impact:** the diagnostics Log section and `[log]` levels cover 54% of call
sites; a `:level pricing trace` will not raise the marketdata upload log or the
pricer rollback error. §3, and the feature is advertised as complete.
**Direction:** a `#[deny]`-style test like the existing `eprintln!` sweep
(`geode-app/src/main.rs:1341-1510` already greps the tree for banned call
shapes) asserting every `tracing::*!` in `crates/*/src` carries `target:`.
There is precedent and a harness for exactly this.

**m17. Timing policy is hard-coded in constants spread over five crates, with no config document.**
`geode-shell/src/frame.rs:29` `FLIP_DEADLINE = 250ms`;
`geode-shell/src/shell/hot_reload.rs:30` `RELOAD_POLL_INTERVAL = 500ms`;
`geode-shell/src/shell/objectdialog/apply.rs:45` `WRITE_DEBOUNCE = 250ms`;
`geode-shell/src/perf.rs:49` `IDLE_CUTOFF = 500ms`;
`geode-blotter/src/tile.rs:46,56` `IN_FLIGHT_AFTER = 50ms`,
`DEFAULT_STALE_AFTER = 15m`; `geode-pricer/src/tile.rs:61,64,78`
`RETRY_AFTER = 1s`, `RETRY_CAP = 30s`, `SAVE_IDLE = 1s`;
`geode-app/src/bridge.rs:216,439` `DEFAULT_PRICING_REFRESH = 30s`,
`CATALOG_RETRY_DELAY = 1s`. Two of these *are* configurable
(`[app] blotter.stale_after` → `bridge.rs:203-211`, `[pricing] refresh`), which
shows the mechanism exists and was applied to two values out of eleven.
**Impact:** Philosophy §5 ("Views, layouts, queries, keymaps, and data sources
are declarative") does not extend to latency policy, and a desk on a slow share
cannot raise the 250 ms flip deadline or the poll interval without a build.
**Direction:** a `[timing]` table read once at startup (restart-required, like
`egress.toml`) covering the flip deadline, poll interval and retry policy;
leave frame-budget constants compiled.

**m18. Two globals fall back to a substituted value when absent, silently.**
`geode-shell/src/clock.rs:5-10` documents the module-facing read as
`cx.try_global::<AppClock>().map(|c| c.0).unwrap_or_else(|| Clock::machine().0)`,
and modules do exactly that at `geode-blotter/src/tile.rs:330,346,898,1154,1403,
1485`, `geode-marketdata/src/tile.rs:939,999`,
`geode-timeseries/src/tile/mod.rs:1250`, `geode-pricer/src/tile.rs:288`,
`geode-diagnostics/src/tile.rs:274`. `SeriesSettings` has the same shape
(`geode-timeseries/src/tile/popups.rs:255` returns early when absent). The
stated reason is a module test fixture that never installed the global, which is
sound; the effect in production would be a tile displaying machine-local time
while its neighbours display the configured zone, with nothing said.
**Impact:** an ambiguity about *which* clock produced a displayed time — the
precise thing §3 forbids — reachable only through a shell bug, and undetectable
if it happens.
**Direction:** keep `try_global` but funnel every module through one
`geode_shell::clock::now_or_machine(cx)` helper that logs once on the fallback
path, so the substitution is observable.

**m19. There is no shared notice facility; the shell's own slot carries one message.**
`geode-shell/src/shell/mod.rs` declares `notice: Option<&'static str>`, and its
only writers are the stack verbs — `shell/input.rs:116,136,143` and
`shell/occupants.rs:451`, all `NOT_IN_A_STACK`. Meanwhile each module built its
own: `geode-blotter/src/tile.rs:194` `error: Option<String>`,
`geode-marketdata/src/tile.rs:612,709` `notice` + `upload_error`,
`geode-timeseries/src/tile/mod.rs:184` `notice`,
`geode-pricer/src/tile.rs:211,218,223` three separate slots (`notice`,
`view_notice`, `save_notice`) with hand-written precedence rules about which may
overwrite which.
**Impact:** five message vocabularies, five presentations, five clearing rules
(pricer's "escape does not clear it" logic exists three times in one file).
**Direction:** part of M1 — one `Notice { text, tone, sticky }` with one paint
and one clear rule; the pricer's three-slot precedence becomes data.

**m20. `config_write` keeps a process-wide writer registry in a `LazyLock<Mutex<HashMap>>`.**
`geode-shell/src/config_write.rs:40-41` plus `TMP_COUNTER: AtomicU64` at line
213. This is the only genuinely ambient mutable state in the workspace outside
GPUI globals (the other statics are benign: `geode-core/src/clock.rs:101`
`OnceLock` for the machine zone, `geode-core/src/panic.rs:23` and
`geode-shell/src/shell/hot_reload.rs:58` thread-locals, and
`geode-chart/src/element.rs:71-85` three thread-local paint counters explicitly
scoped per thread for tests). Keying by absolute path is what makes ordering
correct today.
**Impact:** none today, and the design is deliberate. It becomes relevant for
the separate-windows TODO — two windows in one process share the registry,
which is right, and two *processes* do not, which `shell.md` already notes for
session writes ("Separate processes writing the same session path likewise have
no ordering guarantee").
**Direction:** no change; record it as the intended single-process ordering
authority in `configuration.md` so a future window split does not duplicate it.

**m21. Module file layout diverges in two of five features.**
The established shape is `core/` (pure) + `tile.rs` + `content.rs` +
`delegate.rs`/`header.rs`/`popup.rs`, used by blotter, marketdata and pricer.
`geode-timeseries` uses a `tile/{mod,data,popups,pointer}.rs` directory (the
better shape, per M7) and `geode-diagnostics` uses
`lib.rs` + `sections.rs` + `commands.rs` + `tile.rs` with no `core/` or
`content.rs` at all (its `TileContent` impl lives in `lib.rs:76`). The coding
guides ask for one main responsibility per module and consistent vocabulary
across a family.
**Impact:** minor navigational friction; a newcomer cannot predict where a
module's factory lives.
**Direction:** state the canonical layout in `docs/current/features.md` and move
`geode-diagnostics`' `TileContent`/factory into a `content.rs`.

**m22. Health and degradation reach two of five tiles.**
Grep of the non-test half of each tile for `health`/`degraded`:
`geode-marketdata/src/tile.rs` 2, `geode-timeseries/src/tile/mod.rs` 2,
`geode-blotter/src/tile.rs` **0**, `geode-pricer/src/tile.rs` **0**. The
blotter and pricer show staleness (an age and a `stale` word past
`stale_after`) but never the *source health* that explains it. Philosophy §3:
"source health [is] always visible … A trader must never wonder whether a number
is current." Today the answer is "visible in the diagnostics tile and the status
bar summary", which is one indirection away from the number itself.
**Impact:** a degraded source can feed a blotter that looks merely a little old;
the reason lives in another tile.
**Direction:** the shell already routes everything else a tile needs — add the
worst health of the datasets a tile reads to the value the tile already observes
(it has the publication watch list), and let the shared header of M1 paint one
chip.

**m23. Three near-identical menu row types and three `step`/`snap` implementations.**
`geode-marketdata/src/core/menu.rs:25-40`,
`geode-timeseries/src/core/menu.rs:21-36`,
`geode-pricer/src/popup.rs:129-148` (+ `step` at `popup.rs:164`, `snap` at
`popup.rs:179`). The rulings they each encode are identical and were each
reviewed separately: the highlight skips separators and section headers, a
disabled row takes no fill, a disabled row names its reason, key hints are the
default bindings and a rebind is not reflected. That last one is recorded as a
known limitation in **two** READMEs
(`geode-pricer/README.md`, "The market-data action list has the same
limitation").
**Impact:** a bug fixed in one menu is present in the other two; the rebind
limitation is now a workspace-wide behaviour nobody owns.
**Direction:** one `Menu` in the shared crate of M1 that resolves hints from the
live keymap (the `Chords` global already carries it — `tips.rs:123` shows the
read), which fixes the documented limitation in all three at once.

**m24. `geode-marketdata`'s column widths sit off the rem scale.**
`LABEL_WIDTH`/`CELL_WIDTH` are raw pixels because "`TableDelegate::column` has
no window to read a rem from" (crate README, "Rules this crate pins"), and
`geode-pricer/README.md` records the same gap ("Column widths are pixels and do
not follow the font size"). `shell.md` requires chrome geometry authored against
the rem scale so application zoom stays coherent.
**Impact:** application zoom changes text but not column width in two of the
five tiles — the incoherence §6/design rules exist to prevent.
**Direction:** pass the resolved rem into the delegate at model-build time (both
modules already rebuild their prepared model on a settings change, and the
blotter already re-`refresh`es `TableState` on `on_ui_settings`), rather than
waiting for a window inside `column()`.

### Ideas — the next-phase levers (TODO.md)

**I25. Ingest as a separate daemon process — the seam is right, the wire is missing.**
Readiness: **medium-good.** The request/outcome boundary is already the only
door (`geode-data/src/handle.rs:84-99` `try_send` + refusal counter;
`Request` has 13 variants; `DataEvent` has 13; `EventSink` is a plain
`Arc<dyn Fn(DataEvent) -> bool + Send + Sync>`,
`geode-data/src/service.rs:137`), and `request-delivery.md` already specifies
admission, refusal, supersession and coalescing as if they crossed a process
boundary. Three concrete blockers: (1) **nothing is serialisable** — no
`Serialize` derive on `Request`, `DataEvent`, any `*Params`, `QueryOutcome` or
`Snapshot`; `Snapshot` wraps an Arrow `RecordBatch`
(`geode-core/src/snapshot.rs:388`), so Arrow IPC is the obvious wire and the
type is already positioned for it. (2) `DataServiceConfig` carries three
**trait-object registries** the app fills — `adapters: AdapterRegistry`
(`HashMap<String, Arc<dyn Adapter>>`, `adapter/mod.rs:270-272`),
`documents: DocumentRegistry`, `pricer: PricerConfig` with
`Option<Arc<dyn Pricer>>` (`pricing/mod.rs:17-20`) — so adapter, document-kind
and pricer registration must move into the daemon binary, which also answers
"separate control of ingestion data" by construction. (3) shutdown semantics are
in-process (`handle.rs:101-104` drops the sender to end an idle receive).
**Cheapest seam:** make `EventSink` the process boundary. Derive `Serialize`/
`Deserialize` on `Request` and on `DataEvent` minus `Snapshot`, give `Snapshot`
an Arrow-IPC codec behind its existing private `batch` field, and put a
length-prefixed socket transport behind `DataHandle::send` and behind the sink.
The app's mailbox (`geode-app/src/events.rs`) and bridge need no change: they
already treat every event as arriving asynchronously with coalescing rules.

**I26. Separate windows — closer than it looks.**
Readiness: **good.** `ShellView` is already per-window by design
(`shell.md`: "the retained GPUI entity for **one** window"), and
`geode-app/src/bridge.rs:448,481` already takes a `WindowHandle<Root>`
explicitly rather than assuming one. Blockers: (1) the bridge routes to exactly
one window (`bridge.rs:715` `window.update(...)` with a single handle, and "A
closed window ends the drain on its next event"), so delivery must become
per-window with tile keys namespaced by window; (2) `session.toml` is one
document with `active` + `workspaces.N` (`shell.md`, Session format), so a
second window needs a window dimension in the schema, and `shell.md` already
warns "Separate processes writing the same session path likewise have no
ordering guarantee"; (3) the four globals are process-wide, which is correct for
settings but means a per-window clock or rem is impossible; (4)
`config_write::WRITERS` (m20) is process-wide, which is the *right* answer for
two windows — no change needed.
**Cheapest seam:** make the bridge hold `Vec<WindowHandle<Root>>` and route a
keyed outcome to the window owning that `TileId` (allocate `TileId` globally, as
it already is), then add a `windows.N` level to the session document. Do not
touch the globals.

**I27. Dialog stack — the cheapest of the five.**
Readiness: **good.** Key routing has exactly one gate:
`geode-shell/src/shell/input.rs:568-594` checks `self.modal.is_some()`, calls
`m.on_key`, syncs text, and closes on an unclaimed Escape. `ShellModal`
(`shell/dialog.rs:54-66`) is already a self-contained value — title, builder,
optional title extra, optional key handler — and `open_shell_dialog`
(`dialog.rs:131`) is the single door.
**Cheapest seam:** `modal: Option<ShellModal>` → `Vec<ShellModal>`; `.last()`
receives keys and paints on top; `close_modal` pops. The obstacle is **not** the
modal chrome but M6's nine parallel per-dialog state fields: `open_shell_dialog`
today "closes competing transient surfaces", and a stack means two of those
`Option`s must be `Some` at once. So I27 is cheap *after* M6's `Surface` enum
and awkward before it. Order: M6, then I27. The TODO's "Back button on dialogs"
falls out of the same change (pop one level).

**I28. Shared key bindings — mostly already true; the gap is the fragment layer.**
Readiness: **high for config, low for vocabulary.** Layering already does the
sharing: `keymap.toml` is an ordinary layered document (Builtin → Desk → User,
`architecture.md`, Configuration and persistence), so a desk-level keymap in
`$GEODE_DESK_CONFIG` is inherited today, and `keymap_edit`/`config_write`
already write only the user layer. What is *not* shared is the **vocabulary**:
M8's five copies of the motion bindings, and `ModuleRoster::keymap_fragments`
(`geode-shell/src/module.rs`) which merges per-module fragments that "can never
shadow a shell binding or another module's".
**Cheapest seam:** M8 — a shell-owned `tile-list` context and actions. That
turns "shared key bindings" from a config feature (done) into a vocabulary
feature (one context), and it is the same change that makes a remap one edit.

**I29. First-open wizard — needs a first-run signal, which does not exist.**
Readiness: **low, but cheap.** Nothing in startup detects a first run: grep of
`geode-app/src/main.rs` finds no `first_run` and no emptiness test outside
assertions; a missing session "starts a fresh session without warnings"
(`shell.md`) and a fresh session has no tile at all (only a placeholder hint,
`geode-shell/src/module.rs:216` "double-click or ctrl+k → Add a tile"). The
ingredients are all present: `config_dirs` (`main.rs:1037-1038`) resolves the
user layer, `Config` retains per-path provenance so "is there a user-layer
`sources.toml`?" is already answerable, and `config_write` can create the
document atomically.
**Cheapest seam:** define first-run as "no `sources.toml` in the user **or** desk
layer", and make the wizard the empty-dock surface rather than a modal — it is
the one place a brand-new session already puts the eye, it needs no dialog
stack, and it degrades to today's hint the moment a source exists. Write the
result through `config_write` so the wizard is a config editor, not a new
mechanism (Philosophy §5).

**I30. The shared tile crate is the lever that makes the other four cheaper.**
Not on the TODO, and it should be. M1, M2, M3, M8, m19 and m23 are all one
missing crate: the interaction half of the module contract. It also changes the
cost of the TODO's own module-flavoured entries ("Context menu on blotter",
"Multiple selections in blotter and market data", "Autosize columns (works on
all tiles)" — that last one is *explicitly* a cross-module request with no
cross-module home today).
**Cheapest seam:** start with the smallest three with no state — `popover_surface`,
`Menu` + `step`/`snap`, `Notice` — in a new `geode-tile` crate depending on
`geode-core` + `geode-shell`, with the modules depending on it. That is a pure
move with no behaviour change, it is mechanically verifiable, and it establishes
the home the stateful pieces (`TileQuery`, the windowed cell cache) move into
next.

---

## (d) Systemic patterns

1. **Rules are enforced by prose and repetition, not by types or tests.** The
   dependency rules are enforced by Cargo and hold perfectly. Every *other*
   rule — colours through the doors, blur before drop, arrive on refusal, rem
   geometry, one command one owner — is enforced by a comment at each site and a
   reviewer's memory. Where a rule got a mechanism (the `Delivery` enum forcing
   an exhaustive match; `chip_paint`/`row_paint`/`control::paint`; the
   `eprintln!` sweep test at `geode-app/src/main.rs:1341-1510`) it holds
   silently and cheaply. Where it did not, it is repeated by hand three to five
   times (M1, M2, M8, m23). **The single highest-leverage habit available: when a
   review finding is a mechanism, spend the change on the mechanism.** This
   repo's own memory already contains the ruling ("a reviewer's mechanism
   finding must be ruled on at every site it reaches") — the next step is making
   the site count one.
2. **Comments carry provenance that code should carry.** Dozens of comments cite
   a task, a review round, a finding id or a date ("review fix round 1, MIN-3",
   "user ruling 2026-09-17 and its review's C-1", "Task 5 ruling", "spec §6.5").
   CLAUDE.md asks for the opposite: "A code comment should state the local
   invariant and failure it prevents; it should not require a task number or
   spec section to make sense." Most of these *do* also state the invariant, so
   they are useful; but the citation density means the code's rationale lives in
   an archive a newcomer is told not to read.
3. **Honest self-documentation is the codebase's strongest habit.** Nearly every
   gap I found was already written down: the flat-build 8 ms risk, the pixel
   column widths, the action-menu rebind limitation, the same-day dividend
   reorder hole, the in-memory sheet store, "a budget claim without its
   measurement conditions is not evidence". Three findings above (m23, m24, M3)
   are *quoting the READMEs back*. The failure mode this produces is different
   from the usual one: not hidden debt, but **known debt with no owner** — a
   limitation recorded in two crates' READMEs (m23) belongs to neither.
4. **The pure-core/surface split is real and consistent.** Every module has a
   `core` (or equivalent) tested without a window, and the shell separates
   `tiling`/`keymap`/`frame`/`dialogmode` from the gpui surfaces. This is why
   233k lines written in a month are reviewable at all.
5. **`String` is the universal error and the universal message.** M10 and m19
   are the same pattern seen from two ends: because a failure is a `String`, the
   message is authored at the throw site; because the message is authored at the
   throw site, four copies drifted; because there is no `Notice` type, each
   module invented the precedence rules for showing them.
6. **Tests outweigh implementation in the big files, and live inside them.**
   `marketdata/tile.rs` is 68% tests; `data/service.rs` is 93%;
   `objectdialog/mod.rs` 48%. Coverage is clearly taken seriously, and the
   in-file placement is idiomatic Rust — but combined with M7 it produces files
   no tool and no agent can hold.

## (e) What is done well

1. **The dependency discipline is exemplary.** Fourteen crates, a forbidden
   shell↔data edge, a calculation leaf that no module can name, a composition
   root that is genuinely the only place everything meets — and it all holds in
   `Cargo.toml`, not in a convention. `geode-pricing` depending on
   `geode-core` and *nothing else in the world* is the single clearest signal
   that Philosophy §1 is real.
2. **`Delivery` as an exhaustive enum instead of a second trait method.** The
   comment at `module.rs:227-234` explains it exactly right: a new outcome kind
   refuses to compile until every occupant has an arm. That is a rule with a
   mechanism, and it is why "silence is a bug" holds for deliveries.
3. **Refusal handling is genuinely thought through.** Every submission site
   surfaces the refusal, *and* arrives at the barrier so no other tile hangs,
   *and* clears `acted` so the next frame change is a real retry
   (`geode-blotter/src/tile.rs:765-775` and the three parallel sites). The
   pricer goes further with a documented backoff, a separate notice slot that
   pricing cannot overwrite, and one log line per streak
   (`geode-pricer/src/tile.rs:61-79`). Most codebases return `false` and move on.
4. **Attribution and NULL-over-plausible-wrong.** A non-attributable measure is
   NULL rather than a sum that looks right (`geode-core/src/attribution.rs`, the
   blotter's `f64_at`-only rule). This is the correct instinct for a risk tool
   and it is enforced at the compiler, the snapshot and the delegate.
5. **The bounded, refusable, non-blocking data boundary.** `try_send` under a
   mutex with a refusal counter, `replace_views` stored outside the queue so
   configuration cannot be lost to back-pressure, latest-wins per key with stale
   results dropped, cancellation that is itself refusable and acknowledges
   nothing. `request-delivery.md` documents all of it including what it does
   *not* guarantee. This is production-grade thinking.
6. **Documentation that states failure semantics and limitations.** The session
   recovery table in `shell.md` enumerates thirteen malformed-input cases and
   what each does. `features.md` documents the same-day dividend reorder hole
   including why it cannot be closed without an upstream row key. `data-path.md`
   says discovery compares path/size/time and therefore "an empty poll does not
   prove path accessibility". Documentation that volunteers its own holes is
   rare and it is why this review could be specific.
7. **Struct-of-arrays where it counts most.** `Snapshot`, `DocumentRows`, the
   tree index, `RiskBatch` and the pricer's `sheet` are all columnar, and
   `snapshot.rs` keeps Arrow behind a private field so nothing outside names an
   Arrow type. M3 is a presentation-boundary failure, not a data-layer one — the
   hard half was done right.
8. **Globals were held to four, deliberately, and the rule is written down.**
   `UiSettings`, `Chords`, `AppClock`, `SeriesSettings`, each with a documented
   reason and a `try_global` read pattern; the only other ambient state in 233k
   lines is one writer registry, one `OnceLock` timezone, two thread-locals for
   panic marking and reload counting, and three thread-local paint counters
   scoped for tests. For a GPUI application of this size that is remarkable
   restraint.
