# Geode review — documentation drift and code clarity

Scope: (A) do `docs/current/` guides and crate READMEs describe the code as it
is; (B) code clarity and maintainability across the workspace. Read-only; no
cargo invoked. Every finding was verified by reading the cited code or doc.

## (a) Summary

1. The current guides are unusually accurate for a 230k-line, agent-written
   codebase: of ~100 concrete claims checked, the large majority held exactly,
   including every numeric constant traced (~25), all DuckDB table names, and
   the failure semantics. All 200+ relative doc links and every `#anchor`
   resolve.
2. The real documentation debt is not the guides but **code comments**: 2,472 of
   41,082 comment lines (6.0%) cite task numbers, phase numbers, spec sections,
   review-finding IDs or dated rulings, which CLAUDE.md explicitly forbids.
3. `docs/modules.md` is an orphaned product wishlist sitting in the maintained
   docs tree with no index entry and no inbound link, reading as a description
   of built features.
4. Verified drift is small and concentrated in `features.md` ownership claims;
   the in-flight `update-docs` branch already fixes seven of them. Notably crate
   READMEs are running *ahead* of the guides.
5. Production error discipline is excellent (3 bare unwraps in 38k lines of
   shell; zero `todo!`/`dbg!`/`TODO` comments workspace-wide; a self-enforcing
   guard test banning `eprintln!`). The clarity risk is file size: 23 files over
   2,000 lines, topped by a single 124-method `impl` spanning 4,590 lines.

## (b) Documentation drift findings

| # | Guide | Claim | Code reality | Fixed by update-docs? |
|---|---|---|---|---|
| D1 | configuration.md:352 | "The duration warning currently lists only `s`, `m`, `h`, and `ms`, although the parser also accepts `d` and `y`." | TRUE, still true — `source_config.rs:309` emits "must be an integer with unit s, m, h or ms"; `parse_duration` at `:167-193` accepts `ms/s/m/h/d/y`. Model of a documented gap. | n/a |
| D2 | configuration.md:304-308 | pricing `refresh`: "`"off"` … or any duration. A value that is neither warns … and keeps 30 seconds." | Understates — zero and non-string values also warn. `bridge.rs:216` `DEFAULT_PRICING_REFRESH = 30s`. | **yes** |
| D3 | configuration.md:16-18 | "`%APPDATA%\geode` on Windows or `$HOME/.config/geode` elsewhere" | TRUE — `geode-app/src/main.rs:1043-1046`. | n/a |
| D4 | configuration.md:28-32 | whole-object roots list | **PARTIAL/STALE — `egress` is missing.** `config/merge.rs:19-34` `atomic_depth` includes `"egress" => Some(1)`, tested at `merge.rs:214`. The guide documents `egress.toml` at :255-283 but omits it from the list a config author consults before overriding a target. | no |
| D5 | configuration.md:34-41 | `config_version` must be `1`; unsupported errors+skips, absent warns+assumes 1; builtins bypass | TRUE — `config/load.rs:37-53`, `config/mod.rs:116,257`. | n/a |
| D6 | configuration.md:114-121 | `load_views` order: kind default → view def → dataset presentation → view presentation | TRUE — verbatim in the doc comment at `config/load.rs:72-74`, implemented `:85-110`. | n/a |
| D7 | configuration.md:236-240 | directory defaults: sentinel, 30 s poll, 10 min pending, `latest_risk` | TRUE — `source_config.rs:18` `DEFAULT_POLL=30s`, `:19` `DEFAULT_PENDING_TIMEOUT=600s`, `:153-154`. | n/a |
| D8 | configuration.md:242-244 | subscription defaults `coalesce="500ms"`, `source_time="receive"` | TRUE — `source_config.rs:23`, `:162`. | n/a |
| D9 | configuration.md:246 | `stable_mtime` parses but discovery reports candidates unusable | TRUE — `source/discovery.rs:97-101` returns `Orphaned`. | n/a |
| D10 | configuration.md:325 | "`ctrl` is refused as the alias because it collides with shipped literal Control bindings." | **TRUE** — `geode-shell/src/defaults.rs:454-471` `mod_alias_from_config`: `Some("ctrl")` returns `Severity::Error` with the message "ctrl is reserved for the shipped literal bindings (ctrl+1..9, ctrl+0, ctrl+k, ctrl+/ …); use \"alt\" or \"cmd\"", plus the Alt fallback. Error severity means it does reach the reload rejection decision as configuration.md:218 claims. | n/a |
| D10b | configuration.md:326-328 | "Invalid entries are diagnosed and skipped" | PARTIAL for the alias specifically: `defaults.rs:450-453` documents that "Missing, non-string, and **other string values silently fall back to Alt**". So `mod = "shift"` is silently ignored with no diagnostic, while `mod = "ctrl"` errors. The guide's sentence is about keymap *entries*, leaving the alias's silent-fallback case undocumented. | no |
| D11 | configuration.md:210-213 | watcher waits 500 ms; excludes `session.toml` | TRUE — `shell/hot_reload.rs:30` `RELOAD_POLL_INTERVAL=500ms`; `reload.rs:16` `EXCLUDED_FILENAME="session.toml"`. | n/a |
| D12 | configuration.md:224-226 | "theme-application warnings are currently discarded" | TRUE — `theme.rs:349-355` `apply_from_config` returns `Vec<String>`; caller `shell/hot_reload.rs:264-267` discards it. Honest gap. | n/a |
| D13 | configuration.md:229-231 | `Chords` republished every reload; `UiSettings`/`SeriesSettings`/`AppClock` only on change | TRUE — `shell/hot_reload.rs:228` unconditional; `:238`, `:244-246`, `:254-255` compared. | n/a |
| D14 | configuration.md:198-201 | temp names end in `.tmp` | TRUE — `config_write.rs:249-255` `.{file_name}.{pid}-{counter}.tmp`. | n/a |
| D15 | architecture.md:36-42 | features "do not depend on one another" | TRUE — verified all five `[dependencies]`: blotter/pricer = core+shell+data; marketdata = +widgets; timeseries = +widgets+chart; diagnostics = core+shell. No sibling edges. | n/a |
| D16 | architecture.md:144-147 | CI formats, lints, tests, compiles benches, builds `test-support`, on macOS+Windows | TRUE — `.github/workflows/ci.yml:30-38` runs exactly those five over `matrix.os`. | n/a |
| D17 | data-path.md:60 | "Up to 8 waiting uploads per target … `Err("queue full")` at once" | TRUE — `egress.rs:31` `EGRESS_QUEUE_BOUND=8`, `:177`, `:250`; thread `geode-egress-<name>` at `:181`. | n/a |
| D18 | data-path.md:59 | "Up to 64 waiting requests per source" | TRUE — `ingest/fetch.rs:14` `FETCH_BOUND=64`, used `:60`. | n/a |
| D19 | data-path.md:76-79 | "capped at 250 ms while idle"; "256 paths per source" | TRUE — `ingest/subscribe.rs:38` `MAX_WAIT=250ms` (used `:263-264`), `:189` `UNKNOWN_PATH_CAP=256`. | n/a |
| D20 | data-path.md:30-33 | documents > series > files | TRUE — `ingest/runner.rs:3-4`, `:299-312`. | n/a |
| D21 | data-path.md:61 | files dedupe by path, size, source time | TRUE — `ingest/runner.rs:8`, `:252`, `:268-269`. | n/a |
| D22 | data-path.md:129-131 | sentinel needs `as_of` (RFC 3339) + `columns`, at `<csv>.done` | TRUE — `source/sentinel.rs:1-7,17-19,46,69`. | n/a |
| D23 | data-path.md:136-144 | the five discovery outcomes | TRUE — `source/discovery.rs:26-33` enum; branches `:97-148` match the table row for row, incl. the ":121 remains Pending regardless of pending_timeout" case. | n/a |
| D24 | data-path.md:206 | the `generations` summary table | TRUE — `store/catalog.rs:97` `CREATE TABLE IF NOT EXISTS generations`; separate `file_generations` at `:64`. | n/a |
| D25 | data-path.md:249-251, performance.md:117 | no automatic sweep; "the API is currently called by tests" | TRUE — the only `retention::sweep` caller outside `retention.rs` is `store/document.rs:577`, inside that file's `#[cfg(test)]` (begins `:302`). No production caller. | n/a |
| D26 | data-path.md:167-169 | duplicate adapter registration warns and replaces | TRUE — `adapter/mod.rs:275-280`. | n/a |
| D27 | request-delivery.md:8 | "a 64-entry request channel under a mutex" | TRUE — `handle.rs:30` `REQUEST_BOUND=64`, used `:260,:281`; header `:2`. | n/a |
| D28 | request-delivery.md:80 | "retain the latest 256" diagnostics | TRUE — `geode-shell/src/diagnostics.rs:121` `DATA_DIAGNOSTICS_CAP=256`, used by `geode-app/src/events.rs:161`. | n/a |
| D29 | request-delivery.md:36-39,217 | `replace_views`, `shutdown`, `cancel` | TRUE — `handle.rs:146`, `:217`, `:252`. | n/a |
| D30 | performance.md:31-36 | `cargo run -p geode-app --features profiling` | TRUE — `geode-app/Cargo.toml` `profiling = ["geode-shell/profiling"]`. | n/a |
| D31 | performance.md:41-50 | eight `cargo bench -p <crate>` lines | TRUE for all eight (each crate has the benches and `[[bench]]` entries). **Gap:** `geode-core` (`tree`, `config_merge`) and `geode-demo-data` (`generate`, `dividend`) also have benches and are omitted from the guide's list — while `geode-core/README.md:69` documents `cargo bench -p geode-core` itself. | no |
| D32 | performance.md:60-76 | 13 reference medians | All TRUE, each traced to `docs/perf.md`: 5.0 ms `:131`, 2.51 ms `:779`, 9.56 ms `:1583`, 14.5 ms `:1584`, 1.51 ms `:1683`, 259 µs `:1749`, 285 µs `:1432`, 8.18 ms `:1433`, 116 ns `:1464`, 1.52 ms `:1612`, 6.66 µs `:1613`, 1.85 ms `:1645`. **Caveat:** no date or commit is given, so rows are re-findable only by grepping the number — and 6.66 appears twice in the log with different units (`:1490` a market-data 6.66 ms, `:1613` the pricer's 6.66 µs). | no |
| D33 | performance.md:22-24 | intervals at/above the idle cutoff counted as idle gaps | TRUE — `docs/perf.md:26-29` names `perf::IDLE_CUTOFF` at 500 ms. | n/a |
| D34 | performance.md:16 | chart budget under 2 ms at the 500k cap into 1,600 columns | TRUE, measured 1.51 ms (`perf.md:1683-1685`). | n/a |
| D35 | performance.md:104 | "Density bars … capped at 2,000 quads per frame" | **TRUE** — `geode-chart/src/core/mod.rs:43` `pub const MAX_DENSITY_QUADS: usize = 2_000`; enforced `element.rs:84-102`. | n/a |
| D36 | performance.md:29 | `perf::toggle_overlay` / `perf::reset` | TRUE — registered `defaults.rs:238,244`, bound `mod+shift+p` at `:83`; profiling-only `perf::gpui_overlay`/`perf::dump` at `:329,:335`. | n/a |
| D37 | shell.md:281-286 | session records `active`, `workspaces.N`, `docks.<side>`, `tiles.<id>`, `frame`, `palette.usage`, `config_version=1` | TRUE — `session.rs:238,242,251,280,299,328,333,336,343,346`; node kinds `split`/`leaf`/`stack` at `:15-24`. | n/a |
| D38 | shell.md:293-295 | with both `tile` and `node`, `node` wins | TRUE — `session.rs:479` reads `node` first; docks `:498`. | n/a |
| D39 | shell.md:301-303 | missing version warns+assumes 1; any other present value rejects | TRUE — `session.rs:369-378`. | n/a |
| D40 | shell.md:39, tiling.md | three node kinds; `Tree::layout` computes once | TRUE — `tiling/tree.rs:527`, `tiling/docks.rs:228`. | n/a |
| D41 | shell.md:169-176 | `TileContent`, `Delivery`, `FindEvent`, `StackHandle` | TRUE — `module.rs:187`, `:40`, `:29`, `:107`. | n/a |
| D42 | shell.md:155-157 | flip barrier with a deadline | TRUE — `frame.rs:29` `FLIP_DEADLINE=250ms`. | n/a |
| D43 | shell.md:226-231 | 16 per source, 16 batches, at most 256 | TRUE — `diagnostics.rs:58` `SOURCE_HISTORY_CAP=16`, `:117` `CONFIG_HISTORY_CAP=16`, `:121` `=256`. | n/a |
| D44 | shell.md:117-119 | "Restoring a different query resets selection and scroll to the first match" | **STALE on main** — edit stages now settle onto an *eligible* row (`Draft::is_cursor_stop`). Note `crates/geode-shell/README.md` already documents the new rule, i.e. the README is ahead of the guide. | **yes** |
| D45 | shell.md:88-90, features.md | "a right press focuses exactly as a left one does … arms no drag or double-click gesture" | Consistent across both guides; code route not traced in this pass — UNVERIFIED. | no |
| D46 | input-and-dialogs.md:100-104 | `painted` window default capacity twelve | TRUE — `choice.rs:14` `DEFAULT_CAP=12`. | n/a |
| D47 | input-and-dialogs.md:112-114 | `VimListNav` does not share the matcher's 9999 cap | TRUE — `keymap/matcher.rs:19` `MAX_COUNT=9999` applied `:54`; no cap in `vimnav.rs`. | n/a |
| D48 | input-and-dialogs.md:114-115 | "`vimfind` … current modal filters do not use it as their controller" | TRUE but undersells it — the shell imports only `FindStyle` (`shell/mod.rs:70`, `settings_view.rs:38`, `hot_reload.rs:15`, `input.rs:439`); the find engine's real callers are feature crates (`geode-pricer/src/tile.rs:609,1471-1472,1967`; `geode-marketdata/src/tile.rs:4517,4546,4560`). It is a module-facing API that reads as unused. | no |
| D49 | input-and-dialogs.md:191-193 | "bare digits 1–5 commit presets" | PARTIAL — `asof_rows.rs:274` accepts `(1..=9)`; the bound is the preset table's length, `clock.rs:300-306` (5 entries) further filtered by `at <= now` (`:310`). The guide states a constant where the code has a derived bound. | no |
| D50 | input-and-dialogs.md:172-174 | Ctrl+A adds all filtered, Ctrl+X clears | TRUE — `shell/picker.rs:406`, `:413`. | n/a |
| D51 | input-and-dialogs.md:137-139 | popup shows the first eight candidates | TRUE — `shell/commandline_view.rs:19` `MAX_ROWS=8`. | n/a |
| D52 | input-and-dialogs.md:124-128 | category half weight rounded up; equal scores retain order | TRUE — `palette.rs:88-100` documents `CATEGORY_DIVISOR` and rounding; `listfilter.rs:48-51` relies on stable sort. | n/a |
| D53 | input-and-dialogs.md:88-91, :129 | empty/whitespace query returns all rows; palette queries not trimmed | TRUE — `listfilter.rs:30-40` trims; `palette.rs` does not. The contrast the guide draws is real. | n/a |
| D54 | features.md (pricer) | "`e`, `name`, `new`, and `rm` parse and refuse as not built yet" | TRUE — `geode-pricer/src/core/commands.rs:107`. | n/a |
| D55 | features.md (pricer retry) | 1 s doubling to 30 s | TRUE — `geode-pricer/src/tile.rs:61` `RETRY_AFTER=1s`, `:64` `RETRY_CAP=30s`. | n/a |
| D56 | features.md (pricer save) | "one idle second after its last change" | TRUE — `geode-pricer/src/tile.rs:78` `SAVE_IDLE=1s`. | **partly** (update-docs makes the refusal path honest) |
| D57 | features.md (pricer store) | "**Known limitation:** the sheet store is in memory" | TRUE — `geode-pricer/src/store.rs:12-14`. | no (still true) |
| D58 | features.md (pricer undo) | "100 entries, strictly last-in first-out" | UNVERIFIED — undo depth constant not located (`HALF_PAGE=5`/`FULL_PAGE=10` are unrelated). | no |
| D59 | features.md (timeseries) | pure model "tracks … popups, and session state" | STALE — popup state is not in the pure model. | **yes** |
| D60 | features.md (pricing worker) | "supports cancellation between lines, and answers every line" | STALE/wrong — a cancelled batch need not answer every submitted line. | **yes** |
| D61 | features.md (timeseries fetch) | "Fetch requests ask only for uncovered spans" | STALE (layering) — tile asks for the range; the data tier subtracts coverage. | **yes** |
| D62 | features.md (pricer editor) | "no chord is bound while one is open" | Imprecise — insert-mode bindings leave shell chords reachable. | **yes** |
| D63 | docs/README.md:1-30 | index table of current guides | Complete for `docs/current/` (all 12 listed) — **but `docs/modules.md` has no entry.** See C1. | no |
| D64 | CLAUDE.md:44-52 | `zsh scripts/mutation-check.sh` with three flag forms | TRUE — the only file in `scripts/`. | n/a |
| D65 | README.md:74-87 | 14-crate map | TRUE — matches `ls crates/` exactly. | n/a |
| D66 | README.md:102-105 | "Only `geode-data` opens a file or a socket." | **FALSE as an absolute.** `geode-core` reads config files by design (`config/load.rs`, `Config::read_docs`/`load`), which architecture.md:25-27 and CLAUDE.md:60-62 both state; `geode-shell::config_write` writes them and `session.rs` is written by the shell. Reword to CLAUDE.md's own phrasing: "only `geode-data` owns source I/O and DuckDB connections". | no |
| D67 | geode-core/README.md:17-42 | "What lives here" module table | **INCOMPLETE — omits `clock` (624 lines), `pricing` (319), `nudge` (133).** These are precisely the three modules that exist to serve cross-crate rules: `clock` is the single owner of displayed-time zone, published as `AppClock` and mandated by CLAUDE.md ("Displayed times use `geode_core::clock::Clock`"); `pricing` is "the one place Geode describes an option to a pricing library" (PHILOSOPHY §1 seam); `nudge` exists so marketdata and pricer can share numeric nudging *without depending on each other*. A new engineer looking for any of the three finds no row. | no |
| D68 | geode-data/README.md:66 | "`cargo bench -p geode-data # ingest, query, publish_document, append_series`" | Omits `series_query`, which exists as `benches/series_query.rs` with a `[[bench]]` entry (`Cargo.toml:70`). | no |
| D69 | all docs | relative links and anchors | **TRUE/clean** — across `docs/README.md`, `docs/modules.md`, `docs/perf.md`, `CLAUDE.md`, root `README.md`, `PHILOSOPHY.md`, all 12 `docs/current/*.md` and all 14 crate READMEs: **zero broken file links and zero broken heading anchors.** | n/a |

## (c) Clarity findings by severity

### Major

**C1 — `docs/modules.md` is an orphaned wishlist inside the maintained docs
tree.** `docs/modules.md:1-118` lists mostly *unbuilt* product surfaces
("Scenario Panel", "Vol Watchlist", "Broker Quotes", "Trade History ??? SOPHIS?",
"Sales Credit Reports") with input/output notes and an `# Interactions` section.
It has no entry in `docs/README.md`'s table (D63), no mention in CLAUDE.md's
reading list, and no inbound link anywhere — yet it sits beside guides promised
to describe the system as it is. A new agent opening it will reasonably conclude
these modules exist. Direction: move to `docs/roadmap.md` or the archive; if it
stays, add a one-line header saying it is a product backlog and an index row.

**C2 — 2,472 comment lines (6.0% of 41,082) cite chronology CLAUDE.md forbids.**
CLAUDE.md:141-144: "A code comment should state the local invariant and failure
it prevents; it should not require a task number or spec section to make sense."
Worst cases, where the comment is *meaningless without the archive*:
- `crates/geode-app/src/main.rs:89` — `// NEW-2: see the other process::exit call's own comment above.` A review-finding ID is the whole comment.
- `crates/geode-app/src/main.rs:852` — `// false (Phase 4b Task 1 fix round 1, MIN-8): ShellView::new's own…` four chronology layers in one parenthesis.
- `crates/geode-blotter/src/delegate.rs:566` — `/// (review round 1, Finding 1).` standalone attribution line.
- `crates/geode-blotter/src/delegate.rs:336` — `// (review Minor 1).`
- `crates/geode-app/src/bridge.rs:215` — `/// unreadable value keeps 30 s and says so (planning decision 21).`
- `crates/geode-pricer/src/content.rs:97` — `/// y alone is NOT bound (planning decision 16)…`
- `crates/geode-blotter/src/tile.rs:768` — `// versions (market-data Part 3 Task 6 review, MIN-3, fixed at both sites under the mechanism rule)…`
- `crates/geode-data/src/query/distinct.rs:1231` — `/// D2 (final fix wave, T2 deferred): DistinctParams.column naming a…`
- `crates/geode-shell/src/shell/tests/tiling_keys.rs:311` — `/// End-to-end (ledgered from 1b-ui T3)…`
- `crates/geode-shell/src/shell/render.rs:1144` — `// M10 (3b final review): this same gate also hides whichkey…`
- `crates/geode-app/src/main.rs:926-927` — `// (spec 2026-09-08 add-tile §3.2, retitled by user ruling 2026-09-09)…`
- `crates/geode-blotter/src/tile.rs:327` (also `:1150`, `:1482`) — `// try_global, not the bare cx.global (Task 5 ruling)…`
- `crates/geode-marketdata/src/tile.rs:4682` — `// about a name the spec does declare (final review, T1).`
- `crates/geode-app/src/crash.rs:1-3` — module header as a task ledger: "(Phase 4b). Task 2 added … Task 6 adds the panic hook". Same shape at `crates/geode-shell/src/shell/mod.rs:2-7` ("Chrome (Task 4)", "the tiling tree (Task 3)", "Task 6 wires the real command palette"), recurring at `:122`, `:131`, `:472`, `:1530`.
- `crates/geode-app/src/main.rs:1441-1463` — the most extreme instance: a ~40-line doc comment on the `eprintln!` guard test written as a changelog of its own review ("Fix round 1 (MIN-5) closed three blind spots the first version had…"). The *content* is valuable (it explains why brace-depth counting is exact here and the three shapes that must be handled); only the framing is archival.
Also `shell/tests/objectdialog.rs:5770` ("the controller's M5 ruling"),
`geode-app/src/demo_bus.rs:76` ("(Task 10 brief)"),
`geode-marketdata/src/tile.rs:11727-11789` (a numbered `T1:`…`T4:` block),
and seven "fix round 1, MAJ-1/MIN-4/MIN-5" tags in `geode-app/src/crash.rs`
(`:27,:178,:197,:268,:405,:421,:462`).
Direction: the high-value mechanical sweep is the review IDs
(`MAJ-n`/`MIN-n`/`NEW-n`/`Finding n`/`Minor n`), `planning decision n`, the
`Task n`/`Phase n` module-header ledgers, and `user ruling 2026-MM-DD` — about
600 lines, nearly all of which either say nothing or keep their sentence and
drop the parenthesis. The bare `spec §` tag (792 lines) is milder: roughly half
already state the invariant (see C14) and need only the tag removed.

**C3 — the same chronology is in `Cargo.toml` dependency comments**, where the
reader cannot even reach the archive from code:
`crates/geode-shell/Cargo.toml:42` ("Phase 4a (M6's earlier "test-only" note no
longer holds)"), `:47` ("Phase 4b Task 2"), `:68` ("Phase 4b Task 5"), `:72`
("spec §7.4"); `crates/geode-core/Cargo.toml:15` ("Phase 4b Task 2"), `:23`
("spec §3.3's"), `:45` ("as-of dialog spec §3"), `:55` ("Phase 4c §2.2");
`crates/geode-app/Cargo.toml:13,31,33,36,40,44,49,54` (eight, incl. "Phase 3c",
"line-pricer spec §5.5", "market-data spec §8", "market-data-documents plan
Task 10"); `crates/geode-widgets/Cargo.toml:11` ("as-of dialog spec 2026-09-20
§4"). These are the first thing a contributor reads about why a dependency
exists.

**C4 — `geode-marketdata/src/tile.rs` is 14,729 lines with a single 124-method
`impl` block spanning 4,590 lines.** Production is lines 1–5,405 (tests are
5,407–14,729). Inside that, `impl MarketDataTile` runs from `:712` to `:5303` and
contains **124 methods** — `new`, `key_context`, `holds_focus`, `versions`,
`follows_changed`, `differs_on_followed`, `self_arrive`, `arrive`,
`arrive_and_release`, `requery`, `deliver`, `deliver_upload`, … The seam is
already proven inside this repo: `geode-timeseries` is split into
`tile/mod.rs` (1,257), `tile/data.rs` (338), `tile/pointer.rs` (248),
`tile/popups.rs` (961), plus `commands.rs` (440), `content.rs` (296),
`header.rs` (619), `popup.rs` (1,278) — **no production file over 1,300 lines** —
and `update-docs` documents exactly that four-way responsibility split.
`geode-pricer/src/tile.rs` (5,236) and `geode-blotter/src/tile.rs` (5,044) have
the same shape and the same available remedy. Direction: adopt the timeseries
layout for the other three tiles (data/delivery, pointer, popups, header).

**C5 — `#[allow(dead_code)]` on a production struct field.**
`crates/geode-pricer/src/tile.rs:194` puts it on `frame: Entity<Frame>` with
"Nothing reads it yet: the flip-barrier observer in `new` holds its own handle."
This is the only *unconditional* `dead_code` allow in the workspace — the other
three in that file (`:2458`, `:2572`, `:2713`) sit inside the `#[cfg(test)]`
module beginning at `:2426` and are fine. Direction: drop the field, or if it is
held to keep the entity alive, say *that* — a liveness-only field is a real
invariant worth a comment, and then it is not dead code.

**C6 — test-only helpers in a non-test module behind
`cfg_attr(not(test), allow(dead_code))`.**
`crates/geode-shell/src/shell/settings_view.rs:497,503,509,515` — four items
that exist only for tests but compile into production with the lint silenced.
The crate already has a `test-support` feature that CI builds explicitly, so the
machinery to do this properly exists and is unused at these four sites. (By
contrast `crates/geode-marketdata/src/core/test_fixtures.rs` does it right, with
an inner `#![cfg(test)]` at `:10`.)

**C7 — 21 `.unwrap()` calls on DuckDB row values in a function that already
returns `StoreError`.** `crates/geode-data/src/store/catalog.rs:247-268`
(`lookup_by_path`) unwraps every `row.get(i)`. The positions and types are fixed
by the literal `SELECT` five lines above, and `:250-255` reasons carefully about
the one genuinely nullable column — so this is defensible by construction. But
the documented hazard it meets is real: data-path.md's "Schema limitation"
(`apply_schema` creates tables but does not migrate payload columns) means a
schema drift against an existing database turns these into a **panic** rather
than the `StoreError::Sql` the signature promises. Direction: map the error
(`.map_err(err)?`) at least for the typed columns, so drift degrades to a
diagnostic like every other config/data failure (`geode-core/README.md`:
"A panic on bad config is a defect").

### Minor

**C8 — `configuration.md`'s whole-object list omits `egress`** (D4). One word,
but it is the list a desk-config author reads before overriding a target, and
omitting it implies field-by-field merge where `config/merge.rs:19-34` replaces
wholesale.

**C9 — root `README.md:102-105` contradicts `architecture.md` and `CLAUDE.md`
on the I/O boundary** (D66).

**C10 — `geode-core/README.md` omits `clock`, `pricing`, and `nudge`** (D67) —
the three modules that exist specifically to satisfy cross-crate rules.

**C11 — `performance.md`'s reference table has no date or commit anchor** (D32),
while architecture.md:137 insists "A budget claim without its measurement
conditions is not evidence". Every row is traceable, but only by grepping a
number that is ambiguous in one case. Direction: add a "measured at" column
naming the `perf.md` section.

**C12 — `performance.md`'s bench list omits two crates that have benches**
(D31), one of which documents the command in its own README.

**C13 — `input-and-dialogs.md:191` states a derived bound as a constant** (D49).
Direction: "bare digits select a preset by position" and let the table define
how many.

**C14 — `docs/perf.md`'s section headings are chronology** (`## Phase 2a`,
`## Phase 3c`, `## Market-data documents (spec §5.4/§6, Part 2, Task 11)`,
`## Timeseries (spec §4.4, Part 1)`). Legitimate for an archive, and the file is
correctly labelled one at `perf.md:1-5` — but it means `performance.md`'s
reference values are re-findable only through phase vocabulary, which is what
the current guides exist to free readers from. Ties to C11.

**C15 — `geode-pricer/src/store.rs`'s module header is pure archive
vocabulary.** `store.rs:1-14`: "(line-pricer spec §7.1)", "**The shape is Part
3's** (planning decision 7)", "Part 4's DuckDB store answers `Pending`", "(spec
§7.3)", "(spec §7.2)", "the only implementation until Part 4". The content is
excellent — it explains exactly why `load` can answer `Pending` and why a
zero-row sheet is never saved — but every justification is delegated to a
document the reader does not have. `update-docs` touches this file, so it may
already be improving.

**C16 — three bare `.unwrap()` calls in `shell/commandline_ctl.rs`** (`:130`,
`:148`, `:189`) are the only message-free unwraps in 38,644 lines of shell
production code. All three are guarded by the `let Some(prompt) = … else {
return false }` at `:127-129`, so they are correct; but `:130` could be folded
into that same destructure (`let Some((prompt, tile)) = …`), removing the
unwrap outright, and `:148`/`:189` would read better as `expect` messages naming
the guard, matching the crate's own excellent convention (see F5).

**C17 — `PanelSpec` vs `*Tile` vocabulary overlap.** `geode-marketdata` calls
its on-screen surface a "panel" (`PanelSpec`) while the shell and every other
module call it a tile (`MarketDataTile`, `BlotterTile`, `PricerTile`,
`TimeseriesTile`, `DiagnosticsTile` — 5/5 uniform). `features.md` inherits both
words for the same thing. Low impact; worth one sentence saying `PanelSpec`
describes the *document layout inside* the market-data tile, not a rival to
`Tile`.

**C18 — the `asof` spelling convention is consistent but undocumented.**
`as_of` for fields/values (682), `AsOf` for the type (401), and unseparated
`asof` for module and element names (150: `asof_rows.rs`, `asof_view.rs`,
`asof_chip*`, `asof_tip*`). That is a defensible rule; it is just written down
nowhere, so the next author has a coin flip. One line in the shell README fixes
it.

### Ideas

**C19 — five comments show the shape to copy.** Worth naming in CLAUDE.md as the
positive example, because each carries a `spec §` tag *and* stands alone:
`geode-app/src/main.rs:763` ("invalid config must never stop the app from
starting — error/warn by `Diagnostic::severity`, never a panic"),
`geode-core/src/health.rs:1` ("Data problems are never modal and never fatal: a
failed load leaves live untouched…"), `geode-marketdata/src/core/spec.rs:287`
("four places, because a per-share amount can carry fractional cents a trader
trades on"), `geode-blotter/src/core/commands.rs:65` ("carries a message a user
can act on — never a panic"), `geode-marketdata/src/core/draft.rs:620` ("`base`
must be a CLEAN model of the document this draft's edits are…"). Each loses
nothing if the parenthetical is deleted. One rule — *"delete the tag; if the
sentence then means nothing, rewrite the sentence"* — turns C2 into a mechanical
sweep.

**C20 — `vimfind` is a module-facing API that reads as shell-internal** (D48).
The `geode-shell` README should list it under what a module may call, so the next
feature reuses it instead of writing a fourth `/` search.

**C21 — what would most help a new engineer or agent session**, in payoff order:
(i) A **one-page "where does X live"** map keyed by *task*, not crate: "adding a
`:` command" → that module's `core/commands.rs` + features.md; "adding a config
key" → configuration.md:355-363's five-step checklist (already excellent and
under-advertised); "adding a dialog stage" → configuration-dialogs.md +
`dialogmode`; "adding a tile" → the timeseries module layout as the reference
shape (C4).
(ii) **The comment-style rule with C19's examples inline in CLAUDE.md**, since
the rule is currently stated abstractly and 6% of comments violate it.
(iii) **Mark `docs/modules.md` as backlog** (C1) — the highest-risk document in
the tree, because it reads as a description of built features.

## (d) Counts

### Comment hygiene per crate (chronology-citing comment lines)

Method: awk extractor over 306 files → 41,082 comment lines (`//`, `///`, `//!`,
trailing `//` with string literals stripped, `/* */`); patterns refined to drop
domain-word false positives (`follow-up` as queued publish, `harness` as test
harness, algorithmic `Phase 1:/Phase 2:` in `demo_bus.rs:321/365`, the filename
`geode.2026-09-08.log`). "+ review IDs" adds vocabulary the brief did not list
but that is equally archive-dependent: `MAJ-n`/`MIN-n`/`NEW-n` (105),
`Finding n` (53), `fix round`/`fix wave` (118), `final review`/`whole-branch
review` (114), `planning decision n` (51).

| crate | comment lines | brief patterns | + review IDs | top pattern |
|---|---|---|---|---|
| geode-shell | 18,945 | 797 | **849** | `§` (398) |
| geode-marketdata | 5,945 | 506 | **541** | `§` (312) |
| geode-blotter | 1,891 | 185 | **208** (11%) | `§` (105) |
| geode-pricer | 1,573 | 161 | **200** (13%) | `§` (130) |
| geode-data | 5,313 | 179 | 187 | `§` (125) |
| geode-app | 1,442 | 119 | 137 | `§` (52) |
| geode-core | 2,229 | 123 | 129 | `§` (90) |
| geode-timeseries | 1,818 | 108 | 111 | `§` (70) |
| geode-demo-data | 433 | 51 | 53 (12%) | `§` (21) |
| geode-documents | 481 | 26 | 27 | `§` (23) |
| geode-chart | 456 | 22 | 22 | `§` (18) |
| geode-widgets | 167 | 7 | 7 | `§` (5) |
| geode-pricing | 29 | 1 | 1 | `§` (1) |
| geode-diagnostics | 360 | **0** | **0** | — clean |
| **workspace** | **41,082** | **2,285** | **2,472 (6.0%)** | |

Pattern totals (raw; a line can hit several): `§` 1,350 · `spec §` 792 ·
`2026-` 400 · `task \d` 237 · `ruling` 235 · `Phase \d` 197 · `round \d` 163 ·
`Part \d` 89 · `review round` 50 · `Minor` 28 · `Major` 7 · `Critical` 7 ·
`T\d` 8 · `M\d ` 5 · `4c §` 6 · `mutation entry` 3 · `reviewer` 3 ·
`the spec says` 1. **Zero hits** for `Matthew`, `Claude`, `superpowers`, `PR `,
`commit <sha>`, `as of 2026`, `spec section`, `section \d`, `per the spec`.

Worst files by unique offending lines: `geode-marketdata/src/tile.rs` 314 ·
`geode-shell/src/shell/tests/objectdialog.rs` 192 ·
`geode-shell/src/shell/mod.rs` 127 · `geode-blotter/src/tile.rs` 113 ·
`geode-app/src/main.rs` 81 · `geode-shell/src/shell/render.rs` 61 ·
`geode-pricer/src/tile.rs` 59 · `geode-marketdata/src/core/draft.rs` 49.

### unwrap / expect / panic in production code

Method: per crate, all `src/**/*.rs` excluding `tests/` dirs and `tests.rs` /
`test_support.rs` files, and truncating each remaining file at its first
`#[cfg(test)]`. Caveat: files using inner `#![cfg(test)]` (e.g.
`geode-marketdata/src/core/test_fixtures.rs`, 632 lines) are not excluded by
that rule, so a few crates' "non-test lines" are slightly overstated.

| crate | non-test lines | unwrap | expect | panic! | todo!/unimpl! | unreachable! |
|---|---|---:|---:|---:|---:|---:|
| geode-shell | 38,644 | **3** | 41 | 0 | 0 | 8 |
| geode-data | 11,005 | 21 | 11 | 0 | 0 | 1 |
| geode-marketdata | 10,699 | 2 | 8 | 0 | 0 | 1 |
| geode-core | 8,974 | 0 | 12 | 0 | 0 | 4 |
| geode-pricer | 8,097 | 0 | 12 | 0 | 0 | 0 |
| geode-timeseries | 5,436 | 0 | 5 | 0 | 0 | 0 |
| geode-blotter | 3,382 | 3 | 1 | 0 | 0 | 0 |
| geode-app | 3,143 | 1 | 9 | 0 | 0 | 2 |
| geode-chart | 2,139 | 3 | 1 | 0 | 0 | 0 |
| geode-documents | 1,551 | 0 | 0 | 0 | 0 | 1 |
| geode-demo-data | 993 | 1 | 5 | 3 | 0 | 0 |
| geode-diagnostics | 915 | 0 | 0 | 0 | 0 | 0 |
| geode-widgets | 638 | 0 | 0 | 0 | 0 | 0 |
| geode-pricing | 131 | 0 | 0 | 0 | 0 | 0 |
| **total** | **~96,700** | **34** | **105** | **3** | **0** | **17** |

For comparison, the all-code figures (tests included) are ~4,500 unwraps and
~750 expects — i.e. essentially all of it is test code, which is appropriate.
The 21 `geode-data` unwraps are C7 (`store/catalog.rs`, 12 of them) plus mutex
`.lock().unwrap()` in `adapter/channel.rs:75,101,110,141,200,275,306,318`
(conventional poison propagation). The three `panic!` in `geode-demo-data` are in
a deterministic generator, not a data path.

**Error style:** no `anyhow`, no `thiserror`, no `#[derive(Error)]` anywhere in
the workspace. Five hand-rolled enums with hand-written `Display`/`Error`
(`ClockError`, `EditError`, `LoadError`, `SentinelError`, `StoreError`) at
storage and parsing boundaries; `Result<_, String>` (172 occurrences) where the
error's only consumer is a user-facing message — heaviest in the module crates
(shell 54, marketdata 32, timeseries 31, pricer 30), which is where a refusal is
shown to the trader. Consistent by design; zero error-library idiom drift.

### Leftovers

Workspace-wide: **zero** `dbg!`, **zero** `todo!`/`unimplemented!`, **zero**
`TODO`/`FIXME`/`XXX`/`HACK` comments in any `.rs` file. `println!`/`eprintln!`
appear only in test contexts, enforced by a guard test at
`crates/geode-app/src/main.rs:1341-1510` that walks every `src` tree and fails
on an `eprintln!(` call site outside a `tests/` path or a `#[cfg(test)]` /
`#[cfg(any(test, …))]` block tracked by brace depth.

`#[allow(…)]` totals: `dead_code` 8 (4 shell, all `cfg_attr(not(test), …)`;
4 pricer, 1 of them production — C5); `unused…` **0**; `clippy::` 22
(`too_many_arguments` 15, `type_complexity` 3, `excessive_precision` 2,
`large_enum_variant` 1, `collapsible_if` 1).

### Files over 2,000 lines (23)

| lines | file | note |
|---:|---|---|
| 14,729 | `crates/geode-marketdata/src/tile.rs` | 5,405 production + 9,322 test; one 124-method impl at `:712-5303` (C4) |
| 9,436 | `crates/geode-shell/src/shell/tests/objectdialog.rs` | test |
| 6,189 | `crates/geode-shell/src/shell/objectdialog/mod.rs` | |
| 5,501 | `crates/geode-data/src/service.rs` | the door three guides point at |
| 5,236 | `crates/geode-pricer/src/tile.rs` | same shape as C4 |
| 5,044 | `crates/geode-blotter/src/tile.rs` | same shape as C4 |
| 4,755 | `crates/geode-shell/src/shell/objectdialog/render.rs` | |
| 3,684 | `crates/geode-data/src/query/compile.rs` | |
| 3,554 | `crates/geode-app/src/bridge.rs` | |
| 3,234 | `crates/geode-shell/src/tiling/workspaces.rs` | |
| 2,820 | `crates/geode-shell/src/shell/tests/chrome_and_dialogs.rs` | test |
| 2,790 | `crates/geode-shell/src/tiling/tree.rs` | |
| 2,546 | `crates/geode-timeseries/src/tile/tests.rs` | test |
| 2,519 | `crates/geode-shell/src/shell/tests/keybindings_dialog.rs` | test |
| 2,460 | `crates/geode-shell/src/shell/objectdialog/views.rs` | |
| 2,430 | `crates/geode-marketdata/src/core/matrix.rs` | |
| 2,400 | `crates/geode-shell/src/session.rs` | |
| 2,359 | `crates/geode-marketdata/src/core/draft.rs` | |
| 2,276 | `crates/geode-core/src/schema/mod.rs` | |
| 2,100 | `crates/geode-data/src/query/scope_sql.rs` | |
| 2,086 | `crates/geode-blotter/src/delegate.rs` | |
| 2,048 | `crates/geode-data/src/ingest/runner.rs` | |
| 2,005 | `crates/geode-shell/src/shell/mod.rs` | |

`geode-data` totals 35,014 lines over 42 files. Five of the 23 are test files.
The `geode-shell/src/shell/objectdialog/` directory alone is 6,189 + 4,755 +
2,460 = 13,404 lines across three files.

### Logging and result-type consistency

`tracing::` **exclusively** — 113 call sites across seven crates (shell 44,
data 32, app 22, core 5, marketdata 4, pricer 4, blotter 2), and **zero**
`log::` macros anywhere. `*Outcome` is the uniform name for an operation result
(14 public types: `CatalogOutcome`, `DistinctOutcome`, `FetchOutcome`,
`LoadOutcome`, `PriceOutcome`, `PublishOutcome`, `QueryOutcome`,
`RebindOutcome`, `ReloadOutcome`, `ResetOutcome`, `SeriesOutcome`,
`UnbindOutcome`, `UploadOutcome`, `CaptureOutcome`), with `Delivery` the single
shell-facing wrapper; **no** `Response` or `Reply` type exists. The only noun
carrying both vocabularies is `Upload` (`UploadDelivery` shell-side vs
`UploadOutcome` data-side), which is the deliberate boundary.

## (e) Systemic patterns

1. **The guides are written to a high standard, and the failure mode is
   layering, not inaccuracy.** Where a guide is wrong it is because the
   implementation *moved a responsibility between layers* (D59 popups
   model→tile, D61 coverage subtraction tile→data tier, D44 first-match→
   eligible-row) rather than because a fact changed. That is the drift class to
   watch: a behaviour-preserving refactor still invalidates a guide that names
   owners.
2. **Crate READMEs are running ahead of the guides.** `geode-shell/README.md` on
   main already documents the `Draft::is_cursor_stop` / `move_selection` /
   `settle_selection` rule that `shell.md` still describes the old way (D44) and
   that `update-docs` is in flight to fix. The READMEs are the more current
   artifact, which inverts the reading order CLAUDE.md prescribes ("Start with
   the relevant current guide, then its crate README").
3. **Documented known limitations are accurate and honest.** D1, D9, D12, D25,
   D57 are all real and current, including embarrassing ones (theme warnings
   discarded, no retention scheduler, sheet store in memory). I found **no** case
   of a limitation silently fixed and left stale — the rarer and more valuable
   direction.
4. **Numeric constants are near-perfectly synchronised.** Every one of ~25
   numbers traced (64, 8, 64, 250 ms, 256, 500 ms, 30 s, 600 s, 500 ms, 12,
   9999, 8, 16, 16, 256, 250 ms, 1 s, 30 s, 1 s, 2,000, 2,048…) matched a named
   constant. The habit of *naming* constants (`EGRESS_QUEUE_BOUND`, `MAX_WAIT`,
   `UNKNOWN_PATH_CAP`, `RELOAD_POLL_INTERVAL`, `MAX_DENSITY_QUADS`) is what makes
   this hold and is worth protecting as a rule.
5. **Chronology leaks through every comment channel at once** — Rust comments
   (C2), `Cargo.toml` dependency comments (C3), module headers (C15), and the
   perf log's headings (C14). The codebase treats "why" and "when it was
   decided" as one fact. One rule applied by sweep fixes all four.
6. **`geode-diagnostics` is the control group.** 915 production lines, 360
   comment lines, **zero** chronology citations, zero `dead_code` allows, zero
   unwrap/expect/panic, a complete README, and a guide section that matched every
   claim checked. The standard is achievable inside this workflow.
7. **`geode-timeseries` is the structural control group** (C4): the only tile
   crate that has been decomposed, with no production file over 1,300 lines,
   while the other three tile crates carry 5,000–14,700-line files. The pattern
   to copy already exists in-repo.
8. **Test and production code are not cleanly separated at five sites** (C5, C6)
   — the same smell from opposite directions, both hidden by a lint allow rather
   than a `cfg`, in a workspace that already builds a `test-support` feature in
   CI for exactly this purpose.

## (f) What is done well

- **F1 — Failure semantics are first-class.** "A caller either receives an
  outcome or learns that submission was refused" (architecture.md:71-73), "a
  clean, content-blind poll cannot prove that the last publish was clean"
  (data-path.md), and shell.md:305-325's whole Recovery-and-restoration table
  describe what happens when things go wrong, in a codebase where most
  documentation would cover only the happy path.
- **F2 — Every doc link and anchor resolves** (D69): zero broken relative links
  and zero broken heading anchors across 30 markdown files and 200+ links,
  including cross-crate `../../docs/current/…#anchor` references. The guides
  also link to the code that enforces each contract, and every link followed was
  correct.
- **F3 — `data-path.md`'s queue-and-refusal table and discovery-outcome table**
  are exemplary: every row is a bound or a state *with its refusal behaviour*,
  and each one matched a named constant or enum variant.
- **F4 — Zero `dbg!`, zero `todo!`, zero `TODO`/`FIXME` comments, and a guard
  test that enforces it.** The `eprintln!` walker at
  `geode-app/src/main.rs:1341-1510` is self-enforcing hygiene, correctly handling
  `#[cfg(test)] mod tests;`, `#[cfg(any(test, feature = "test-support"))]`, and
  the call-shape-vs-prose distinction.
- **F5 — `expect` messages name their invariant.** Almost every one of the 105
  production `expect`s reads like a proof obligation: "just ensured [ui] is a
  table" (`fontsize.rs:126`, `linenumbers.rs:144`, `theme.rs:435`,
  `log_persist.rs:43`, `series.rs:143`, `vimfind.rs:91`, `tileadd.rs:114`),
  "contains(anchor) implies a root" (`tiling/tree.rs:303,482,797`), "removing one
  member of a stack never empties the tree" (`:362`), "index came from
  position()" (`keymap_edit.rs:265`), "every typed name was checked against the
  list above" (`objectdialog/groupings.rs:282`). This is CLAUDE.md's comment rule
  applied to panics, and it is the single most reviewable habit in the codebase.
- **F6 — Uniform vocabulary and tooling.** `tracing::` exclusively with zero
  `log::`; 14 `*Outcome` types with no `Response`/`Reply` synonym; `*Tile` for
  all five module occupants; no `anyhow`/`thiserror` mix — one deliberate error
  strategy throughout.
- **F7 — `configuration.md:355-363`'s "Maintaining configuration" checklist** is
  the best onboarding artefact in the repo: five numbered steps from "define and
  validate it in the owning typed document" to "test merge, invalid input, user
  override, and reload behavior at the lowest layer that proves the contract".
- **F8 — The current/archive split is real and navigable.** `docs/perf.md` is
  labelled an archive at its head, `docs/README.md` states the policy plainly,
  and no current guide I read contained an "as built" section or review
  chronology — the prose guides are clean of the problem that saturates the code
  comments.
- **F9 — The dependency rules hold in fact, not just on paper** (D15), verified
  from `Cargo.toml` rather than prose: no feature depends on a sibling,
  `geode-shell` and `geode-data` are mutually independent, and `nudge` exists in
  core precisely so two features can share code without depending on each other.
- **F10 — The in-flight `update-docs` branch is doing exactly the right work.**
  Of the clear stale findings, D2, D44, D59, D60, D61, D62 and part of D56 are
  already fixed there, all of them ownership/layering corrections of the kind
  pattern 1 predicts.

## Passes not completed

- Functions over ~200 lines were not systematically enumerated (the largest
  single structure was found by another route — C4's 124-method impl). File-level
  sizing is complete.
- Per-crate README invariant spot-checks were completed for `geode-core`,
  `geode-data`, `geode-shell`, `geode-chart` only; the other ten READMEs were
  checked for module-map completeness but not claim-by-claim.
- Duplicated test-helper names across test modules, and builder-vs-pub-field
  struct style, were not surveyed.
- `pub` items with zero external users were not sampled.

TODO.md verification (partial, by grep — each a zero-hit search, so **not yet
implemented**, consistent with a live backlog): `autosize`/`auto_size`/`autofit`
(no hits in any `.rs`); blotter multi-select (`multi.?select`,
`selection_anchor`, `extend_selection` — none in blotter or marketdata); blotter
context menu (`context_menu`, `MouseButton::Right` — none in blotter, though
`geode-timeseries` and `geode-pricer` both ship `.`/right-click action menus, so
the pattern to copy exists); scope-expression suggestions (`complet`/`suggest` —
none in `shell/scope_expr_view.rs`, matching that guide's "Validation is
syntax-only"); dialog stacking (no `dialog_stack`; `shell/dialog.rs:172-173` and
`shell/mod.rs:692` have only the single `overlay_return_to_filter` flag).
One item is **accurate and diagnosable**: "if i type `scope clear` i get no
matches because its `clear scope`" — `palette.rs:88` `fuzzy_match` is a single
in-order subsequence match over `title + space + category`, with no per-word
scoring, so out-of-order words genuinely cannot match; the neighbouring "right
order increases score" is already half-built as the category-half-weight rule
(`palette.rs:93-95`). The `[pres]`/`[builtin]` badge question corresponds to
`shell/objectdialog/render.rs:3350` (`Destination::Presentation => "pres"`), so
the `pres` chip exists and the open question is only whether a `builtin`
counterpart joins it.
