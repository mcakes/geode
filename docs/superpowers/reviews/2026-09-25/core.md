# geode-core review

Scope: `crates/geode-core` (31 files, ~17.8k lines including tests). Read-only
review against `docs/PHILOSOPHY.md`, `CLAUDE.md`, the crate README, and
`docs/current/{configuration,typed-documents}.md`.

## Summary (5 lines)

1. The crate is in good shape: it really is I/O-free apart from the two
   documented entry points, it holds no gpui or DuckDB type, and the
   struct-of-arrays discipline in `snapshot`/`tree`/`document` is genuine.
2. The dominant systemic defect is a *silent-fallback* class in the typed
   readers: several malformed-but-plausible config shapes are dropped with no
   diagnostic at all, which contradicts "silence is a bug" and the crate's own
   stated rule that every parse failure degrades to a `Diagnostic`.
3. The layered-merge core (`config/merge.rs`) is small and correct for the
   documented cases, but `atomic_depth` is a hardcoded doc-name table with no
   link to the readers that depend on it, and `record_provenance` is O(n) per
   leaf, making the merge super-linear in leaf count on the keystroke path.
4. `config_version` is filtered by nine readers individually and *not* by
   `SchemaSpec::from_doc`, so a `datasets.toml` carrying the conventional
   version stamp yields a phantom dataset — verified against the shipped demo
   config, which omits the stamp from `datasets.toml` only by luck.
5. Comment quality is unusually high (failure-mode-first, measurements
   recorded), but ~95 comments cite spec sections/phase numbers that CLAUDE.md
   asks to keep in the archive, and a handful of large readers
   (`schema/mod.rs`, `view.rs`) have outgrown single-function comprehension.

---

## Critical

### C1. `SchemaSpec::from_doc` does not skip `config_version`, minting a phantom dataset

**Location:** `crates/geode-core/src/schema/mod.rs:219` (loop head), compared
with `src/view.rs:426`, `src/scopes.rs:49`, `src/dimensions.rs:50`,
`src/groupings.rs:44`, `src/source_config.rs:326`, `src/egress_config.rs:96`,
`src/colour/mod.rs:179`, `src/view.rs:652`, `src/view.rs:931`.

Every other top-level-object reader in the crate begins its loop with
`if name == "config_version" { continue; }`. `SchemaSpec::from_doc` does not.
`config_version = 1` is an integer, so `ds_value.get("family")` and
`ds_value.get("columns")` both return `None`; family defaults to
`Family::Measures`, the "no `[columns]` table" branch at `schema/mod.rs:374`
fires a *warning* naming `datasets.config_version`, and because an empty
measure dataset is not dropped (`schema/mod.rs:393` only drops columnless
*documents*), a `DatasetSpec { name: "config_version", .. }` is pushed into
`SchemaSpec.datasets`.

That value then flows to real consumers: `geode-data/src/service.rs:546` calls
`store.apply_schema(ds)` for every dataset (creating metadata for it),
`geode-shell/src/shell/objectdialog/views.rs:134` and
`objectdialog/sources.rs:107` offer dataset names to the trader as pick
options, and `geode-shell/src/shell/mod.rs:329` iterates datasets to build the
pickable-column list. The shipped `examples/demo-config/datasets.toml` happens
to carry its `config_version` *inside a comment header* rather than as a key —
`grep -n config_version examples/demo-config/*.toml` lists `app.toml`,
`dimensions.toml`, `groupings.toml`, `views.toml` but **not**
`datasets.toml` — so the repo's own demo does not hit it. Any desk or user
`datasets.toml` written to the documented convention (`docs/current/configuration.md`
says the stamp is expected and an *absent* stamp only warns) will.

**Impact:** a spurious dataset named `config_version` appears in dataset
pickers, receives `apply_schema`, and produces a permanent bogus warning at
`datasets.config_version`. Worse, `load_layer` warns when the stamp is
*missing*, so the configuration that avoids the bug is the one the loader
complains about.

**Direction:** add the same `config_version` guard at `schema/mod.rs:219`. This
is a one-line fix; the deeper fix is factoring the guard into one shared helper
(see S1) so the tenth reader cannot forget it.

---

## Major

### M1. `Scope::and_then` intersection is order-dependent and quadratic in values

**Location:** `crates/geode-core/src/scope/mod.rs:61-105`, specifically line 81
(`existing.values.retain(|v| sel.values.contains(v))`).

The intersection is a `Vec::contains` inside `retain`, so composing two layers
each selecting *n* values on one dimension costs O(n²) string comparisons. The
frame scope composes on every scope edit and the result feeds
`geode-data/src/query/scope_sql.rs` and `distinct.rs`. A book selection is
small today, but a picker-driven "select all" over a real underlying list (the
same list that pushes an ENUM key to UInt16, i.e. >255 values, per
`snapshot.rs:578-588`) makes this thousands of `String` comparisons on the UI
thread per composition.

Separately, the doc comment at lines 56-60 admits a real asymmetry: an *empty
outer* selection is cloned and then intersected to empty (setting `impossible`),
while an *empty inner* selection is skipped. So `a.and_then(&b)` and
`b.and_then(&a)` differ for unnormalized inputs, and a reader-produced empty
selection (`scopes.rs:62-75` produces exactly that when a `values` key is not
an array) silently turns a saved scope into a contradiction that selects
nothing. The comment documents the trap rather than closing it.

**Impact:** an O(n²) allocation-touching path in the interaction loop
(PHILOSOPHY §6 calls per-frame heap churn a reviewable defect), plus a
"selects nothing" outcome reachable from a malformed `scopes.toml` value.

**Direction:** intersect through a `HashSet`/`BTreeSet` built once from the
inner selection; and normalize away empty selections at reader boundaries
(`scopes.rs`) so `and_then` never sees one, making the asymmetry unreachable
rather than documented.

### M2. Saved-scope reader silently discards constraints on malformed fields

**Location:** `crates/geode-core/src/scopes.rs:60-81`.

`dimensions` that is not a table is skipped with no diagnostic (line 60,
`and_then(|v| v.as_table())` — the `None` arm does nothing). A `values` entry
that is not an array becomes an *empty selection* (lines 63-70,
`.unwrap_or_default()`), and a mixed array keeps only the strings
(`filter_map(|v| v.as_str())`), both with no diagnostic. `text` that is not a
string is dropped silently (line 77-81). The module doc at lines 25-28 states
this plainly: "These silent fallbacks can remove constraints from the loaded
scope."

This is the exact failure PHILOSOPHY §3 forbids: the trader sees a scope chip
that says one thing while the query behind it constrains another. And per M1,
the empty selection produced here can compose into `impossible`, so a typo in
`scopes.toml` can silently turn a saved scope into "matches nothing" — a
plausible wrong answer rather than an explicit error, which CLAUDE.md
specifically ranks as more serious.

**Impact:** a hand-edited `scopes.toml` can narrow or blank a trader's data
with no visible signal, on a document the desk is expected to share.

**Direction:** every one of these fallbacks already has a natural diagnostic
path (`warn(path, m)` is already in scope at line 41). Emit a warning for a
non-table `dimensions`, a non-array `values`, a non-string array element, and
a non-string `text`; and drop rather than empty a malformed selection.

### M3. `record_provenance` is O(provenance) per leaf, making merge super-linear

**Location:** `crates/geode-core/src/config/merge.rs:97-101`.

Every leaf write calls `record_provenance`, which does
`prov.retain(|p, _| p != path && !p.starts_with(&prefix))` — a full scan of the
`BTreeMap` plus a `format!("{path}.")` allocation, for *every* leaf of *every*
layer. Merging *L* leaves is therefore O(L²) comparisons with L string
allocations, and `merge_table` also allocates a `format!("{path}.{key}")`
child path per key (line 70) whether or not provenance is recorded.

This sits directly on the keystroke path: `benches/config_merge.rs:1-14`
documents that a config-dialog field edit re-merges the whole document set
per edit, and `geode-shell/src/shell/objectdialog/apply.rs:427` and `:443`
confirm it (`Config::from_docs(shell.services.config.all_docs())`). The bench's
own comment (lines 36-43) notes the largest document — the builtin keymap — is
*absent* from the fixture because it lives in `geode-shell`, so the measured
number is an acknowledged underestimate; `BUILTIN_KEYMAP`
(`geode-shell/src/defaults.rs:24`) is ~75 lines / ~62 assignments in the
always-active section alone, on top of a 7.8 KB `datasets.toml` and 6.8 KB
`views.toml`.

**Impact:** the retain-per-leaf is quadratic work inside the <8 ms pure-UI
budget, and it is measured on a fixture that deliberately excludes the biggest
contributor.

**Direction:** the retain exists only to drop stale descendant entries when a
higher layer replaces a subtree. Record provenance into a trie/prefix map
keyed by path segments (or clear descendants once at the replacement site,
where the subtree is already in hand), rather than scanning the whole map per
leaf. Also reuse one `String` buffer for the child path instead of `format!`
per key.

### M4. `atomic_depth`'s whole-object list is an unlinked hardcoded table

**Location:** `crates/geode-core/src/config/merge.rs:15-37`, versus
`docs/current/configuration.md` ("Top-level entries in `views`,
`view_presentation`, `dataset_presentation`, `layouts`, `groupings`, `scopes`,
`datasets`, `sources`, `dimensions`, `colours`, `pricer_views`, and
`overrides` replace whole named objects").

The list in code and the list in the guide agree today (I compared them
entry-for-entry; `egress` at line 27 is in the code list and documented in its
own section). But nothing ties a reader to its merge semantics: a new document
kind gets a typed reader in this crate and its atomicity is a separate edit in
a different file, with no compile-time or test-time link. The consequences of
getting it wrong are asymmetric and invisible — a document that *should* be
atomic but is not will recursively merge a user's partial override onto the
desk's definition, producing a hybrid object (e.g. a source with the user's
`adapter` and the desk's `topics`) that neither layer authored and that no
diagnostic describes.

**Impact:** the failure mode is a silently synthesized configuration object,
which per CLAUDE.md is worse than an explicit error. Latent rather than live.

**Direction:** make atomicity a property declared beside the reader — e.g. a
`const ATOMIC_DEPTH: Option<u32>` on a small `Document` trait each reader
implements, or at minimum a test that asserts the `atomic_depth` list equals a
list the readers themselves publish. A test that merely restates the literal
list (as the current `merge.rs` tests do, one document at a time) cannot catch
an omission.

### M5. `Clock::machine()` memoises a fallback warning that is then reported once per reload

**Location:** `crates/geode-core/src/clock.rs:100-105` and `:174-238`.

`Clock::machine()` caches `(Clock, Option<String>)` in a `OnceLock` — correct,
and the doc comment (lines 91-99) explains the render-path reason well. But
`from_config` at lines 207-213 pushes that cached warning as a fresh
`Diagnostic` on *every* call, and `from_config` runs on every config load and
every hot reload. On a machine whose zone cannot be read, the diagnostics tile
accumulates the same warning per reload rather than showing one standing
condition.

Also at line 275: `degrees: (degrees % 360.0) as f32` is applied *after* the
`(0.0..=360.0)` range check at line 250, so the modulo can only ever fire for
exactly `360.0` (mapping it to `0.0`, which is harmless) — dead arithmetic that
reads as if out-of-range values were being normalized.

**Impact:** duplicate diagnostics on a degraded-zone machine; a misleading
modulo that suggests wrap-around handling that cannot occur.

**Direction:** have `from_config` report the machine-zone warning only when the
diagnostic set is being built fresh (or let the consumer dedupe — `Diagnostic`
already derives `PartialEq` specifically so "unchanged diagnostic batches can be
deduplicated", per `config/mod.rs:42-44`, so the consumer may already handle
it; worth confirming). Drop the `% 360.0`.

### M6. `parse_as_of` tries six formats by trial-parse, and `%H:%M` shadows nothing but is order-fragile

**Location:** `crates/geode-core/src/query.rs:240-267`.

The function attempts `%H:%M`, `%H:%M:%S`, `%Y-%m-%d`, `%Y-%m-%d %H:%M`,
`%Y-%m-%d %H:%M:%S`, then RFC 3339, taking the first that parses. The ordering
is load-bearing and undocumented as such: `%H:%M` must precede `%H:%M:%S`
(chrono's `%H:%M` would reject `12:30:45` anyway, so this pair is safe), and
`%Y-%m-%d` must precede the datetime forms (again safe, since `%Y-%m-%d` rejects
trailing text). So the current order is correct — but nothing records *why* a
reorder would break, and the failure mode of a wrong order is that a typed
as-of silently resolves to a different instant than the trader meant.

The error message at line 266 lists the accepted forms, which is good. But
there is no test-visible statement of the precedence contract.

**Impact:** an ordering invariant with no guard, on the one input that decides
which generation of data the trader is looking at.

**Direction:** state the precedence rule in the doc comment as an invariant
("each earlier format rejects every string a later one accepts, so the order is
free" — or if that is not true for some pair, say which), and add a test that
pins one string per format to the instant it must produce.

### M7. `saved_scopes_from_doc` recovers the offending column by string-splitting its own diagnostic

**Location:** `crates/geode-core/src/scopes.rs:106-129`, specifically line 117:
`let column = d.message.split('\'').nth(1).unwrap_or("");`

To decide whether a validation diagnostic belongs on a dimension row or the
expression field, this reader parses the *message text* produced by
`Scope::validate` (`scope/mod.rs:128-166`) to extract the column name. The
comment at lines 108-116 is admirably honest about the coupling ("every message
shape it produces"), but it is still a structural dependency on prose: any edit
to a `Scope::validate` message that moves or adds a single-quoted run
mis-addresses the diagnostic, and nothing fails.

Note also that `min_by_key(|d| d.len())` at line 104 picks the dataset with the
*fewest* complaints, so on a multi-dataset schema the reported diagnostics may
describe a dataset the trader never had in mind.

**Impact:** silent diagnostic mis-addressing on a message edit; the dialog puts
the error on the wrong row.

**Direction:** `Scope::validate` should return the offending column as a field
rather than only in prose — either a structured error type, or populate
`Diagnostic.path` at the validate site (it currently sets `path: None`,
`scope/mod.rs:133`) and let the caller rewrite the prefix.

### M8. Whole-object atomic merge drops required fields with no diagnostic at the merge layer

**Location:** `crates/geode-core/src/config/merge.rs:73` and
`:88-92`, with the consequence documented at
`docs/current/configuration.md` ("Overriding one source therefore requires its
complete configuration, including required fields; omitted fields do not
inherit from the lower-layer source").

This is deliberate and documented, and `merge.rs`'s tests pin it
(`atomic_doc_replaces_named_object_whole` at :146,
`egress_is_atomic_by_target_name` at :213). The gap is that the *merge* stage
says nothing when a higher layer's object is strictly smaller than the one it
replaced — the trader finds out later, from whichever typed reader happens to
notice a required field is missing, at a diagnostic path that points at the
user layer's file without mentioning that a complete desk definition was
discarded. For `views` the reader complains about a missing `dataset`
(`view.rs:465-467`); for `sources` it complains about a missing `dataset`
(`source_config.rs:357-365`); but for a merely *incomplete* override the result
is a quietly narrower object.

**Impact:** the most confusing configuration failure available in this system —
"I only changed one field and the whole view changed" — arrives as an unrelated
downstream diagnostic or as no diagnostic at all.

**Direction:** at the atomic replacement site, compare key sets and emit an
informational diagnostic naming the keys the higher layer dropped. The merge
currently returns no diagnostics at all (`MergedDoc` has no diagnostics field),
so this needs a channel; it is worth one.

---

## Minor

### N1. `check_object_name` rejects nothing that TOML itself would reject, and misses `[`/`]`

**Location:** `crates/geode-core/src/config/mod.rs:272-281`.

Rejects empty, `config_version`, whitespace, `.`, and `"`. A name containing
`[`, `]`, `'`, `#`, or `=` passes validation but cannot be written as a bare
TOML key; it would have to be quoted, and the writers
(`geode-shell::config_write`) round-trip through `toml_edit`. Since the
function's stated purpose is "validate and persist the same name", the
character set should match what a bare key permits.

**Direction:** reject anything outside TOML's bare-key set (`A-Za-z0-9_-`)
rather than blacklisting four characters.

### N2. `parse_duration` accepts `d` and `y` but the shared warning message does not mention them

**Location:** `crates/geode-core/src/source_config.rs:171-198` (parser accepts
`ms`, `s`, `m`, `h`, `d`, `y`) versus `:308-312` (`"'{key}' must be an integer
with unit s, m, h or ms"`).

`docs/current/configuration.md` already records this as a known gap ("The
duration warning currently lists only `s`, `m`, `h`, and `ms`, although the
parser also accepts `d` and `y`"). The same parser is reused for series
retention windows (`schema/mod.rs:319`), where `30d`/`5y` are the *documented*
spellings (`schema/mod.rs:321-322`, `:326-327`) — so the message is wrong
precisely where the units matter most.

**Direction:** one shared constant for the accepted-unit list, referenced by
both messages.

### N3. `parse_duration`'s last-char unit scan breaks on multi-byte input

**Location:** `crates/geode-core/src/source_config.rs:183-185`.

`let (idx, unit) = s.char_indices().last()?;` then `let digits = &s[..idx];`.
This is UTF-8-safe (char_indices gives a boundary), so no panic — good. But the
`digits.bytes().all(is_ascii_digit)` check then rejects, meaning a value like
`"30 s"` (with a non-breaking space) produces the generic "must be an integer
with unit" warning rather than anything pointing at the whitespace. Cosmetic,
noted only because the surrounding code is otherwise careful about messages.

### N4. `Aggregate::sql` builds a `String` per call and is the only SQL-shaped thing in core

**Location:** `crates/geode-core/src/schema/column.rs:52-59`.

`fn sql(self, expr: &str) -> String` returns `format!("sum({expr})")`. It is a
tiny SQL-text builder living in the "no DB concepts" crate, beside
`ColumnType::sql` (`column.rs:19-28`, returning `&'static str`) and
`CompareOp::sql` (`scope/expr.rs:21-31`, `&'static str`). The `&'static str`
ones are arguably vocabulary; the `String`-building one is code generation.

**Direction:** return the function name as `&'static str` and let the compiler
in `geode-data` assemble the call, matching its two siblings.

### N5. `Token::name`'s `Chart(_)` arm silently maps every out-of-range index to `chart.5`

**Location:** `crates/geode-core/src/colour/mod.rs:76-83`.

`Token::Chart(1..=4)` map to their own names; `Token::Chart(_)` maps to
`"chart.5"`. So `Chart(9)` names itself `chart.5`, and `Token::parse` (line 63,
`ALL.into_iter().find(|t| t.name() == s)`) would round-trip `chart.5` to
`Chart(5)` — a silent renumbering. `Tokens::get` at line 363 independently
clamps with `n.clamp(1, 5)`. Two different out-of-range policies for one
newtype.

**Direction:** make `Chart` carry a validated 1..=5 (a small enum or a
constructor returning `Option`), so neither the clamp nor the catch-all arm is
needed.

### N6. `interpolate_hue`'s `t <= 0.0` early return makes exact anchors a special case

**Location:** `crates/geode-core/src/colour/mod.rs:374-395`.

`let t = (h - ANCHOR_DEGREES[i]) / 60.0; if t <= 0.0 { return ring[i]; }`. Since
`i = floor(h/60) % 6`, `t` is in `[0, 1)` by construction, so `t <= 0.0` means
exactly `t == 0.0` — the exact-anchor case. Returning `ring[i]` directly
(rather than the OKLCH round-trip, which is lossy) is the *point*, and the doc
comment says so ("Exact anchor angles return the anchor unchanged"). But
writing it as `t <= 0.0` rather than `t == 0.0` suggests defensiveness against
a negative `t` that `rem_euclid` has already excluded.

### N7. `GroupingSlots::from_doc` reports a non-numeric key at a path built from the key itself

**Location:** `crates/geode-core/src/groupings.rs:47-57` with the path built at
`:41` (`format!("groupings.{slot}")`).

For an unparseable key the diagnostic path is `groupings.<the bad key>`. If the
key contains a `.` (e.g. `groupings.toml` holding `1.2 = [...]`), the emitted
path is ambiguous with a nested path, and `Config::explain`'s ancestor walk
(`config/mod.rs:236-248`) truncates at `.`. Comment at lines 34-35 acknowledges
the spelling is retained deliberately; the ambiguity is not mentioned.

### N8. `GroupingSlots::set` returns `bool` and silently no-ops on a bad slot

**Location:** `crates/geode-core/src/groupings.rs:102-111`.

`set` returns `false` for a slot outside 1..=9 or an empty grouping. A `bool`
return is easy for a caller to drop; nothing in the type prevents it. Given
that slot numbers are a fixed 1..=9 vocabulary, a `Slot(u8)` newtype validated
once at the boundary would make `set` infallible.

### N9. `DerivedDimensions::from_doc` accepts a dimension with no `values` map at all

**Location:** `crates/geode-core/src/dimensions.rs:75-109`.

If `values` is absent or not a table, the dimension is pushed with an *empty*
map (line 76's `if let Some(map) = ... ` has no `else`), with no diagnostic. A
derived dimension that maps nothing is inert — every source value falls through
unmapped — so a typo in the `values` key produces a dimension that appears in
groupings and scope pickers (it is `known` to `groupings.rs:31`) and silently
groups everything into one bucket. Contrast the careful many-to-one conflict
check at lines 86-99, which does diagnose.

Also: `base_column` (lines 38-43) is documented as "a single lookup, not
recursive resolution", but nothing rejects a dimension whose `from` names
another derived dimension, so a two-hop chain silently resolves to the
intermediate name rather than the base column.

### N10. `ScopeSemantics::meet` has an `unreachable!` reachable only by editing its sibling arm

**Location:** `crates/geode-core/src/attribution.rs:86-94`.

The `NotApplicable` arm at line 91 is `unreachable!("handled by the arm
above")` — true today, because the match arm at :75-77 catches every
`NotApplicable` pair first. It is a correct-by-adjacency `unreachable!`: moving
or reordering the arms above turns it into a panic. Since this runs on the
query worker inside a `panic::contained` boundary, the failure is a contained
panic rather than a crash, but it is still a panic where a `Direct`-equivalent
fallthrough would do.

**Direction:** restructure so the impossible case cannot be spelled — e.g.
match on `(self.rank(), other.rank())` — or return the conservative weakest
value instead of panicking.

### N11. `Snapshot::depth_of_row` clamps silently; `TreeIndex::build` clamps again

**Location:** `crates/geode-core/src/snapshot.rs:675-680` and
`src/tree.rs:86`.

`depth_of_row` returns `None` when the depth exceeds `grouping.len()`
(`.filter(|d| *d <= self.grouping.len())`), and `TreeIndex::build` then does
`snapshot.depth_of_row(r).unwrap_or(0).min(max_depth)` — so a row whose
`row_depth` exceeds the grouping length is treated as depth 0, i.e. a *root*,
i.e. a grand-total row. A compiler bug emitting an over-deep `row_depth` would
therefore surface as extra grand-total rows rather than an error, and
`TreeIndex::unplaced` (which exists precisely to surface structural surprises in
the footer, `tree.rs:207-211`) would not count it.

**Direction:** count clamped rows toward `unplaced` so the footer says so, per
"silence is a bug".

### N12. `Scope::columns()` allocates a `Vec<String>` per call and is called per render

**Location:** `crates/geode-core/src/scope/mod.rs:110-121`, called from
`geode-shell/src/scopebar.rs:94`
(`scope.columns().into_iter().next().unwrap_or_default()`).

`columns()` clones every dimension name plus every expression column name into
a fresh `Vec<String>`; the scopebar call site then takes only the *first*
element and drops the rest. `Expr::columns()` (`scope/expr.rs:60-64`) already
returns borrowed `&str`, so the `String` allocation is `columns()`'s own choice.

**Direction:** return an iterator of `&str` (the expression half already
borrows), leaving the owned-Vec construction to callers that need it.

### N13. `parse_literal` accepts `-`/`+` anywhere inside a number run

**Location:** `crates/geode-core/src/scope/expr.rs:375-393`.

The digit scan accepts `c.is_ascii_digit() || c == '.' || c == '-' || c == '+'`
in any position, then defers to `f64::parse`. So `1-2` is consumed as one token
and rejected as "not a number" with the caret at the *start* of the run, rather
than parsing `1` and reporting an unexpected `-`. Harmless (it is an error
either way), but the caret lands somewhere the trader did not type wrong.

Note the scan also accepts an exponent-free form only: `1e5` tokenizes as
`1` followed by identifier `e5`, giving "unexpected trailing input". Worth
stating in the module doc as a deliberate grammar boundary.

### N14. Spec-section and phase-number comments throughout, against CLAUDE.md

**Location:** ~95 occurrences of `Phase N`, `§`, `Task N`, or `spec ` in
non-test code. Densest: `src/snapshot.rs` (25), `src/query.rs` (15),
`src/document.rs` (9), `src/pricing.rs` (8), `src/series/mod.rs` (8),
`src/tree.rs` (7), `src/attribution.rs` (7), `src/health.rs` (4),
`src/log/mod.rs` (4), `src/clock.rs` (5). Also review-round markers:
`src/panic.rs:1` ("Phase 4b Task 6 fix round 1, MAJ-1"),
`src/log/mod.rs:263` ("MIN-3 (fix round 1)"), `:306` ("Task 4 fix round 1,
MIN-10"), `:321` ("fix round 1, MIN-6"), `src/query.rs:130` ("MIN-5, final
review"), `src/clock.rs:92` ("final review Minor 2"), `:241` ("final review,
Minor 3"), `Cargo.toml:13` ("Phase 4b Task 2"), `Cargo.toml:30` ("spec §3.3").

CLAUDE.md: "A code comment should state the local invariant and failure it
prevents; it should not require a task number or spec section to make sense."
Most of these comments *do* state the invariant and failure — the citation is
additive rather than load-bearing — so this is a cleanup, not a defect. The
review-round markers (`MAJ-1`, `MIN-3`, `fix round 1`) carry no information for
a future reader and are the clearest candidates for removal.

### N15. `test-support` feature comment cites a task number in `Cargo.toml`

**Location:** `crates/geode-core/Cargo.toml:9-19`.

The feature's rationale is excellent (why `Snapshot::for_tests` must live in
`snapshot.rs`), but "(Phase 4b Task 2)" at line 13 is archive material. Same
for the `toml` dependency comment's "spec §3.3" at line 30 — though that block's
substance (the workspace-wide `preserve_order` audit, and the one real
order-dependence it found in `keymap::build`) is exactly the kind of comment
CLAUDE.md asks for and should be kept.

### N16. `first_duplicate_rows` sorts then scans, but the "first" it reports is not the sort's first

**Location:** `crates/geode-core/src/document.rs:267-291`.

The function sorts row indices by axis value, then walks `windows(2)` looking
for equal neighbours, tracking `found` as the pair with the smallest *earlier*
index (lines 280-289). The doc comment (lines 259-263) explains this
carefully. But the loop does not break on the first match — it scans every
window to the end even after a duplicate is found, because a *later* window
might hold a numerically smaller earlier-index. On a document with many
duplicates this is a full extra pass; on the common no-duplicate case the cost
is the same either way. Correct, just not obviously minimal.

### N17. `DocumentRows::validate` performs O(columns) name lookups per column

**Location:** `crates/geode-core/src/document.rs:205-253`.

Each of the four validation loops calls `ds.column(name)` or scans
`ds.columns.iter()` — and `DatasetSpec::column` is itself a linear
`iter().find()` (`schema/mod.rs:85-87`). For a document with *v* values and *a*
attributes against a dataset of *c* columns this is O((v+a)·c). The comment at
lines 188-193 shows the author was explicitly cost-conscious about the
duplicate-row check on this same path ("the receiver thread runs this per
message"), which makes the surrounding linear scans the inconsistency.

**Direction:** if this path is genuinely per-message, build a name→spec index
once per dataset (or memoise it on `DatasetSpec`).

### N18. `DatasetSpec` accessors are linear scans used from per-query paths

**Location:** `crates/geode-core/src/schema/mod.rs:85-87` (`column`),
`:212-214` (`SchemaSpec::dataset`), and the derived helpers `grains()` (:92),
`carries()` (:119), `groupable_columns()` (:150), `dimensions_at()` (:135).

`groupable_columns` at :164-169 computes `self.grains()` (which allocates, sorts
and dedups a `Vec<Grain>`) and then calls `self.carries(*g, &c.name)` for every
(grain, column) pair — and `carries` itself calls `self.column(column)`, another
linear scan. So `groupable_columns()` on a dataset of *c* columns with *g*
grains is O(c²·g). It is called from grouping editors and column completion
(per its own doc comment at :145-149), which are interactive.

**Direction:** these are all pure functions of an immutable spec — compute them
once when the `DatasetSpec` is built and store the results, or memoise. The
struct is already `Clone` and rebuilt only on reload.

### N19. `Grain` key columns are four hardcoded `const` arrays with duplicated prefixes

**Location:** `crates/geode-core/src/schema/grain.rs:14-43`.

`K`, `K_INSTRUMENT`, `K_UNDERLYING`, `K_PAIR` repeat the same four leading
names, and the module doc (line 2) states the load-bearing invariant: "Their
storage keys form a prefix chain." Nothing enforces it. A typo in one array
silently breaks the prefix relation that `carries()` (`schema/mod.rs:119-131`)
and `attribution_of` (`attribution.rs:117-155`) both depend on.

**Direction:** derive the narrower arrays as prefixes of `K_PAIR` (e.g.
`&K_PAIR[..4]`), which makes the chain structural; or assert it in a test.

### N20. `Frequency::index` and `BucketRule::next` use `expect` on a self-lookup

**Location:** `crates/geode-core/src/series/mod.rs:85-90` and `:136-139`.

`Self::ALL.iter().position(|f| *f == self).expect("every frequency is in ALL")`
— true by construction, but it is a runtime search plus a panic path for what a
`match` would answer with no lookup. Same shape twice. These are on the tile's
`b`/frequency-step keys, so they are interaction-path code.

**Direction:** `match self { M1 => 0, M5 => 1, ... }`, removing both the scan
and the `expect`.

### N21. `Clock::in_zone_named` is a `#[doc(hidden)]` panicking API in the public surface

**Location:** `crates/geode-core/src/clock.rs:80-83`.

`pub fn in_zone_named(name: &str) -> Clock` panics on an unknown IANA name. The
doc comment justifies it for downstream *tests* ("every call site names a real
zone literally"). But it is a `pub` function on a `pub` type with no
`cfg(test)`/feature gate, so nothing stops production code from reaching it —
and the crate already has a `test-support` feature (`Cargo.toml:19`) that is
exactly the right home for it.

**Direction:** move it behind `#[cfg(any(test, feature = "test-support"))]`,
like `Snapshot::for_tests` and `config::test_support::config_from`.

### N22. `EgressSpec::address` aborts the whole target on the first bad `documents` entry

**Location:** `crates/geode-core/src/egress_config.rs:60-86`.

`read_documents` returns `Err` on the first entry naming a non-document dataset
or holding a non-string address, dropping the entire target. The comment at
lines 57-59 says this "mirrors how `source_config` aborts a source on its first
invalid required field" — but a `documents` *map* is not a required field; it is
a collection of independent entries, and one typo takes down every other upload
address for that target. Compare `DatasetPresentationSpec::from_doc`
(`view.rs:963-1010`), which diagnoses and skips per column.

**Direction:** diagnose and skip the offending entry, dropping the target only
if no entry survives (which the existing empty-map error already covers).

### N23. `Snapshot::from_batches` is the only fallible constructor and its check is name-equality

**Location:** `crates/geode-core/src/snapshot.rs:411-426`.

The meta/batch agreement check compares the two name vectors for equality,
allocating two `Vec<&str>` to do it, and returns an `ArrowError::SchemaError`
carrying a formatted message. Allocating on the success path of a
per-query constructor is minor; using `ArrowError` as the error type leaks the
"Arrow is an implementation detail" promise of the module doc (lines 3-5) into
the *signature* — every caller of `from_batches` in `geode-data` must name
`arrow::error::ArrowError`.

**Direction:** define a small `SnapshotError` (or `String`) so the module's own
promise holds at the boundary; compare names with `zip`/`all` rather than two
allocations.

---

## Ideas

### I1. One `Document`-reader trait to collapse nine hand-copied preambles

Every top-level-object reader repeats: skip `config_version`, build an
`at(suffix)` path closure, build a `bad(suffix, msg)` diagnostic closure, check
`value.as_table()`, push "not a table". Compare `view.rs:437-454`,
`view.rs:657-674`, `view.rs:934-951`, `source_config.rs:326-332`,
`egress_config.rs:96-102`, `colour/mod.rs:182-206`, `dimensions.rs:55-69`,
`scopes.rs:41-58`, `groupings.rs:36-57`. C1 is exactly the bug this duplication
invites. A `for_each_object(doc, "views", |name, table, diag| ...)` helper
would make the `config_version` skip, the path prefix, and the not-a-table
diagnostic structural rather than remembered — and would be the natural place
to hang the atomicity declaration from M4.

### I2. Make `Diagnostic.path` a typed path rather than a `String`

`Diagnostic.path` is built by `format!` at roughly 60 sites and consumed by
dialogs that match on its shape (`geode-shell/src/shell/objectdialog`). The
crate already pays for this coupling twice: M7's message-splitting, and N7's
ambiguous path when a key contains a `.`. A `DiagPath(Vec<PathSegment>)` with a
`Display` impl would keep the wire format identical while making
"document, object, field" checkable and unambiguous.

### I3. Give `Scope` a normalizing constructor so `and_then`'s asymmetry is unreachable

M1 and M2 are two faces of one thing: `Scope` permits states its own
composition operator handles asymmetrically (an empty `DimensionSelection`).
A `Scope::new`/`normalize` that drops empty selections at construction, with
the readers routed through it, makes the documented asymmetry a non-question and
removes the "malformed config becomes a contradiction" path.

### I4. Intern column and dataset names

`String`-keyed lookups and `String`-cloning accessors recur throughout:
`DatasetSpec::column` (linear over `String`), `Scope::columns` (clones),
`Scope::and_then` (clones + `Vec::contains` over `String`),
`SchemaSpec::dataset` (linear), `ColumnMeta.name` (owned per column per
snapshot), `groupable_columns`/`dimensions_at` (rebuild per call). An interned
`ColumnId`/`DatasetId` assigned once at schema load would turn every one of
these into an integer compare or an index, and directly serves PHILOSOPHY §6.
This is the single change with the widest performance reach in the crate.

### I5. Move `format.rs` and `nudge.rs` closer to their two users, or accept them as vocabulary

`format::format_number` and `nudge::nudge_text` are pure display helpers shared
by the blotter, market-data panel and line pricer "which may not depend on each
other" (`nudge.rs:5`). That is a legitimate reason for them to be here. Worth
recording explicitly in the README's module table (which lists `format` but not
`nudge`, and does not list `pricing`, `series`, `panic` or `log` under "What
lives here" either) so the crate's membership rule stays stated rather than
inferred.

### I6. Test the merge contract, not the merge examples

`config/merge.rs` has 8 tests, each pinning one documented behaviour by
example. What is missing is the *contract*: that every document named in
`atomic_depth` behaves atomically (M4), that provenance's recorded ancestor
always answers `explain` for every leaf beneath it, and that
`from_docs(all_docs())` is idempotent for an arbitrary document set (the
existing `from_docs_merges_exactly_as_load_does` at `config/mod.rs:332` checks
two fixed documents). Given that merge semantics are the highest-consequence
pure logic in the crate and sit on the keystroke path, 8 example tests is thin
relative to the risk. The same applies to `Scope::and_then`: 14 tests in
`scope/mod.rs`, none covering the empty-outer-selection asymmetry its own doc
comment describes.

---

## Systemic patterns

1. **Silent fallback on malformed-but-plausible config.** `scopes.rs` (M2),
   `dimensions.rs` (N9), and the `filter_map(|v| v.as_str())` idiom used in
   `view.rs:474`, `:492`, `groupings.rs:61`, `source_config.rs:447`, `:567` all
   discard bad array elements with no diagnostic. The crate's README promises
   "Every parse failure degrades to a `Diagnostic` and skips the offending
   input"; the *skip* half is honoured everywhere, the *diagnostic* half is not.
   This is the one pattern I would fix as a pattern rather than site by site.

2. **Duplicated reader preamble, one copy missing a line.** Nine readers
   hand-copy the same five-step opening; the tenth (`SchemaSpec::from_doc`) is
   missing the `config_version` skip, which is C1. Duplication that has already
   produced one bug.

3. **Prose as a data channel.** `scopes.rs:117` parses its own diagnostic text
   (M7); `Diagnostic.path` is a formatted string matched on by dialogs (I2);
   `check_object_name`'s rules are stated in a message rather than derived from
   TOML's key grammar (N1). Each is small; together they mean message edits are
   behaviour edits.

4. **Linear scans over `String`-keyed collections on interactive paths.**
   `DatasetSpec::column`, `SchemaSpec::dataset`, `Scope::and_then`,
   `groupable_columns`, `record_provenance`'s retain, `DocumentRows::validate`.
   The crate is scrupulous about allocation in `snapshot`/`tree`/`document`'s
   *hot* loops and comparatively relaxed everywhere else — including the config
   keystroke path, which `benches/config_merge.rs` itself identifies as
   budget-bound.

5. **`unreachable!`/`expect` used as adjacency proofs.** `attribution.rs:92`,
   `series/mod.rs:89`, `:137`, `clock.rs:50`, `:55`, `query.rs:230`,
   `view.rs:880`, `snapshot.rs:745`. The `const`-context ones
   (`clock.rs:50/55`, `query.rs:230`) are compile-time and fine. The rest are
   runtime panics whose correctness depends on a sibling line not moving.

6. **Comments that record measurement history.** `snapshot.rs:54-94`
   (concat/dictionary, "this comment has been wrong twice"),
   `snapshot.rs:578-588` (ENUM key widths, "this is the third place to find out
   about it"), `clock.rs:91-99` (OS call on the paint path). This is the
   *positive* systemic pattern and it is unusually good — see below.

---

## What is done well

- **The crate boundary genuinely holds.** No gpui or DuckDB type anywhere; the
  only `std::fs` in non-test code is `config/load.rs:13`/`:23`, exactly the two
  documented entry points. `Arrow` is confined to `snapshot.rs` apart from its
  leak in `from_batches`'s error type (N23), and the `test-support` feature
  exists specifically so downstream tests need not name it — a real
  architectural investment, explained in `Cargo.toml`.
- **Failure-mode-first comments.** `snapshot.rs`'s dictionary-concat doc
  (lines 54-94) records two superseded conclusions and the measurements that
  refuted them; `document.rs:131-142` explains why an empty document is refused
  by naming the two destructive outcomes; `health.rs:5-16` documents why `Ord`
  must not be used for rollup and names the wording that invited the bug. This
  is the standard the rest of the workspace should be held to.
- **Diagnostic addressing.** Nearly every reader attaches a
  `datasets.<ds>.columns.<col>.<key>`-shaped path at the deepest field it
  honestly knows, with explicit comments about not guessing deeper
  (`view.rs:432-436`, `schema/mod.rs:941-943`). The "one mistake, one
  diagnostic" test at `config/load.rs:410-416` is a good example of pinning that
  discipline.
- **Partial-result validation.** The schema reader's distinction between
  dropping a column, clearing a flag, and dropping a dataset is carefully
  thought through and individually justified (e.g. `schema/mod.rs:727-750` on
  why a document column named `book` must fail the whole dataset rather than
  just the column).
- **Struct-of-arrays throughout.** `TreeIndex` is CSR with `u32` indices,
  `DocumentRows` is columns, `SeriesResult` is columns, and
  `Snapshot::for_tests`'s `dictionary_fixture` was itself optimised because the
  fixture became the bottleneck (`snapshot.rs:732-735`). PHILOSOPHY §6 is
  visibly applied rather than cited.
- **Both parsers bound their recursion.** `series/expr.rs:78-83` documents
  `MAX_DEPTH` *and* `MAX_TOKENS` and explains precisely why one is insufficient
  (left-deep folds nest nothing) — the rare case of a stack-overflow analysis
  done properly rather than assumed.
