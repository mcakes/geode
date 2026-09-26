# Market-Data Generation Identity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give a document generation an identity a draft can compare — source
time *and* store generation — so a corrected republish at the same source time
puts an open draft `Behind` instead of silently re-pointing its edits, and keep
an I64 cell's value exact from typed text through `:bump` to upload.

**Architecture:** `Freshness::generation` already sits beside `as_of` in every
snapshot's provenance. It is written as a database-global counter, written as
literal `0` for historical reads, and read by nothing. This branch gives it a
meaning — the generation actually read, per dataset — and then makes the
market-data draft's base the pair `(source time, generation)` rather than the
time alone. Separately, the numeric cell path is retyped so an integer column
never passes through an `f64`.

**Tech Stack:** Rust 2024, DuckDB, GPUI (`gpui-kit`), `toml` with
`preserve_order`.

**Spec:** `docs/superpowers/specs/2026-09-26-geode-silent-wrong-data-design.md`
(§4; §1 and §2 bind the whole spec)

## Global Constraints

- The principle: **identity over position, and refuse rather than guess.** A
  plausible wrong value on screen or sent to the desk is worse than an explicit
  refusal.
- A republish is not an error and must not discard work. `Behind` is the
  existing state meaning "the document moved under an open draft"; it already
  offers `:rebase` and `:revert`. Invent no third behaviour.
- An unknown generation is **not** evidence that the data did not move. Where
  no single generation names a read, report `None` and fall back to the source
  time — never a placeholder integer.
- Displayed times go through `geode_core::clock::Clock`; never `chrono::Local`.
- Every behaviour change gets its mutation-harness entry, and every harness
  anchor this branch invalidates gets re-anchored in Task 4.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  and `cargo test --workspace` must pass at the end of every task.
- No new dependency. `Freshness` keeps `Debug, Clone, PartialEq, Eq`.

---

## File Structure

**Task 1 — the store reports the generation it read**

- Modify `crates/geode-core/src/snapshot.rs` — `Freshness::generation` becomes
  `Option<i64>` with a doc comment saying what `None` means; two test literals
  at `:1259-1266`.
- Modify `crates/geode-data/src/store/catalog.rs` — two new readers,
  `live_generation` (one partition, mirroring `live_source_time`) and
  `dataset_generation` (one dataset, the view path's counterpart to
  `dataset_as_of`).
- Modify `crates/geode-data/src/query/compile.rs` — `CompiledQuery` gains
  `resolved_generation: Option<i64>`.
- Modify `crates/geode-data/src/query/document.rs` — the as-of arm already
  holds the single `ResolvedGeneration` it pinned; report its `gen_id`.
- Modify `crates/geode-data/src/query/distinct.rs`, `crates/geode-data/src/query/pool.rs`
  (test helper) — the new field, `None`.
- Modify `crates/geode-data/src/query/read.rs` — all four provenance arms.
- Modify the six test/bench `Freshness` literals:
  `crates/geode-blotter/src/tile.rs:2357`,
  `crates/geode-marketdata/benches/matrix.rs:35`,
  `crates/geode-marketdata/src/core/matrix.rs:1104`,
  `crates/geode-marketdata/src/core/test_fixtures.rs:35` and `:526`,
  `crates/geode-marketdata/src/tile.rs:4642`.

**Task 2 — a draft's base is a pair**

- Modify `crates/geode-marketdata/src/core/draft.rs` — new `DocumentBase` type
  with `differs_from`; `Draft::base` and `DraftState::Behind.newer` carry it;
  every edit door takes `&DocumentBase`; `to_toml`/`from_toml` carry the
  generation.
- Modify `crates/geode-marketdata/src/core/matrix.rs` — `MatrixModel.source_time`
  becomes `MatrixModel.base: Option<DocumentBase>`, built by one new
  `pub(crate) fn base_of(snapshot) -> Option<DocumentBase>`.
- Modify `crates/geode-marketdata/src/tile.rs` — delete the duplicate
  `source_time_of` in favour of `core::matrix::base_of`; `edit_base`,
  `attr_edit_base`, `echo_of`, `Echo::Differs.newer`, and the two base-equality
  sites take the pair.

**Task 3 — an I64 cell keeps its type**

- Modify `crates/geode-marketdata/src/core/draft.rs` — `parse_cell` returns
  `Result<Value, String>`; `parse_attr` delegates both numeric arms to it;
  `numeric_edit` returns the typed `Value`; `bumped` takes and returns `Value`
  and does integer arithmetic for an I64 column.
- Modify `crates/geode-marketdata/src/tile.rs` — `commit_cell_edit` and the
  three `bumped` call sites.

**Task 4 — harness and docs**

- Modify `scripts/mutation-check.sh` — re-anchor ten entries, add six.
- Modify `docs/current/features.md`, `docs/current/data-path.md`,
  `crates/geode-marketdata/README.md`, `crates/geode-data/README.md`.

---

### Task 1: Provenance reports the generation it actually read

**Files:**
- Modify: `crates/geode-core/src/snapshot.rs:28-33`, `:1256-1270`
- Modify: `crates/geode-data/src/store/catalog.rs` (after `live_source_time`, which ends at `:331`)
- Modify: `crates/geode-data/src/query/compile.rs:30-41`, `:894-901`
- Modify: `crates/geode-data/src/query/document.rs:97`, `:100-135`, `:158-167`
- Modify: `crates/geode-data/src/query/distinct.rs:108-115`
- Modify: `crates/geode-data/src/query/pool.rs:475-485`
- Modify: `crates/geode-data/src/query/read.rs:79-136`
- Modify: `crates/geode-blotter/src/tile.rs:2357`, `crates/geode-marketdata/benches/matrix.rs:35`, `crates/geode-marketdata/src/core/matrix.rs:1104`, `crates/geode-marketdata/src/core/test_fixtures.rs:35`, `:526`, `crates/geode-marketdata/src/tile.rs:4642`
- Test: `crates/geode-data/src/store/catalog.rs` (tests module), `crates/geode-data/src/query/read.rs` (tests module)

**Interfaces:**
- Produces:
  - `geode_core::snapshot::Freshness { dataset: String, as_of: Option<String>, generation: Option<i64> }`
  - `Catalog::live_generation(&self, dataset: &str, batch: &str, book: Option<&str>) -> Result<Option<i64>, StoreError>`
  - `Catalog::dataset_generation(&self, dataset: &str) -> Result<Option<i64>, StoreError>`
  - `CompiledQuery::resolved_generation: Option<i64>`
- Consumes: nothing from a later task.

**Why the field changes type.** `generation: i64` is written in exactly two live
places, both as `catalog.latest_gen_id()` — `max(gen_id)` over the *whole
database*, so an unrelated dataset's load moves it — and as literal `0` in both
as-of arms. Nothing reads it. Once Task 2 makes it load-bearing, `0` becomes a
lie. `Option<i64>` makes "no single generation names this read"
unrepresentable-as-wrong.

- [ ] **Step 1: Write the failing catalog test**

Add to `crates/geode-data/src/store/catalog.rs`'s tests module, next to
`live_source_time_is_per_partition_not_per_file` (`:806`):

```rust
    #[test]
    fn live_generation_is_the_partitions_newest_and_moves_on_a_same_time_republish() {
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());
        // Two generations of ONE partition at the SAME source time: the
        // corrected republish a draft must be able to tell apart.
        for generation in [1i64, 2] {
            let mut r = FileGeneration {
                path: format!("/src/risk_v{generation}_BK000.csv").into(),
                ..record("BK000", &["BK000"], ts("2026-08-30T07:00:00Z"))
            };
            r.gen_id = generation;
            cat.record(&r).unwrap();
        }
        // Another partition, newer, must not answer for this one.
        let mut other = record("BK001", &["BK001"], ts("2026-08-30T14:00:00Z"));
        other.gen_id = 9;
        cat.record(&other).unwrap();

        assert_eq!(
            cat.live_generation("risk_snapshot", "BK000", Some("BK000"))
                .unwrap(),
            Some(2),
            "the partition's own newest generation, not the database's"
        );
        assert_eq!(
            cat.dataset_generation("risk_snapshot").unwrap(),
            Some(9),
            "the dataset's newest generation across its partitions"
        );
        assert_eq!(
            cat.live_generation("risk_snapshot", "NOSUCH", Some("BK000"))
                .unwrap(),
            None,
            "a partition that has never loaded has no generation"
        );
        assert_eq!(
            cat.dataset_generation("nosuch_dataset").unwrap(),
            None,
            "a dataset that has never loaded has no generation"
        );
    }
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p geode-data live_generation_is_the_partitions_newest -- --nocapture`
Expected: FAIL — `no method named live_generation`.

- [ ] **Step 3: Add the two catalog readers**

Insert immediately after `live_source_time` (which closes at
`crates/geode-data/src/store/catalog.rs:331`):

```rust
    /// One partition's newest live generation — [`Self::live_source_time`]'s
    /// identity, over the same rows under the same filters.
    ///
    /// A corrected republish keeps its source time and takes a new
    /// generation ID, so this is the only thing that distinguishes the two
    /// for a reader holding unsent work over the older one. `None` before
    /// the partition's first load.
    pub fn live_generation(
        &self,
        dataset: &str,
        batch: &str,
        book: Option<&str>,
    ) -> Result<Option<i64>, StoreError> {
        // The same dataset/batch/book scoping `live_source_time` explains:
        // file stems can match across datasets, and the bookless partition
        // is its own.
        let (sql, params): (&str, Vec<duckdb::types::Value>) = match book {
            Some(b) => (
                "select max(fg.gen_id) from file_generations fg
                 join file_books fb on fb.file_id = fg.file_id
                 where fg.dataset = ? and fg.batch = ? and fb.book = ?
                   and coalesce(fg.archived_only, false) = false",
                vec![
                    dataset.to_string().into(),
                    batch.to_string().into(),
                    b.to_string().into(),
                ],
            ),
            None => (
                "select max(fg.gen_id) from file_generations fg
                 join file_books fb on fb.file_id = fg.file_id
                 where fg.dataset = ? and fg.batch = ? and fb.book is null
                   and coalesce(fg.archived_only, false) = false",
                vec![dataset.to_string().into(), batch.to_string().into()],
            ),
        };
        self.conn
            .query_row(sql, duckdb::params_from_iter(params), |r| r.get(0))
            .map_err(|source| StoreError::Sql {
                statement: sql.into(),
                source,
            })
    }

    /// A dataset's newest live generation, across every partition.
    ///
    /// The maximum, where [`Self::dataset_as_of`] takes the minimum, because
    /// the two answer different questions. `as_of` reports how stale an answer
    /// is and so must name its stalest input; the generation reports whether
    /// this is the same data as last time, and a minimum would not move when
    /// a single partition republished — exactly the change a reader of this
    /// field exists to see. `None` before the dataset's first load.
    pub fn dataset_generation(&self, dataset: &str) -> Result<Option<i64>, StoreError> {
        let sql = "select max(gen_id) from file_generations
                   where dataset = ? and coalesce(archived_only, false) = false";
        self.conn
            .query_row(sql, [dataset], |r| r.get(0))
            .map_err(|source| StoreError::Sql {
                statement: sql.into(),
                source,
            })
    }
```

- [ ] **Step 4: Run the catalog test to verify it passes**

Run: `cargo test -p geode-data live_generation_is_the_partitions_newest`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-data/src/store/catalog.rs
git commit -m "feat(data): the catalog reports a partition's and a dataset's live generation"
```

- [ ] **Step 6: Widen `Freshness::generation` to `Option<i64>`**

In `crates/geode-core/src/snapshot.rs:28-33`, replace the struct with:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Freshness {
    pub dataset: String,
    /// RFC 3339, or `None` when the dataset has never loaded.
    pub as_of: Option<String>,
    /// The store generation this answer was read from, or `None` when no
    /// single generation names the read: a dataset that has never loaded, or
    /// a historical view read, whose era resolves one generation *per
    /// partition* and so has no scalar identity.
    ///
    /// Source time is not an identity on its own — a corrected republish
    /// keeps its source time and takes a new generation ID — so a reader
    /// deciding whether the data under it moved must compare this too.
    /// `None` is not evidence that it did not; it means unknown.
    pub generation: Option<i64>,
}
```

Then fix every construction site by wrapping the integer in `Some(...)`:

```bash
# The six test and bench literals, all of the form `generation: <int>,`.
for f in crates/geode-core/src/snapshot.rs \
         crates/geode-blotter/src/tile.rs \
         crates/geode-marketdata/benches/matrix.rs \
         crates/geode-marketdata/src/core/matrix.rs \
         crates/geode-marketdata/src/core/test_fixtures.rs \
         crates/geode-marketdata/src/tile.rs; do
  perl -0pi -e 's/generation: (\d+),/generation: Some($1),/g' "$f"
done
git diff --stat
```

Confirm the diff touches exactly eight literals (two in `snapshot.rs`, one each
elsewhere except `test_fixtures.rs`, which has two) and no production code.

- [ ] **Step 7: Add `resolved_generation` to the compiled plan**

In `crates/geode-data/src/query/compile.rs`, inside `CompiledQuery` (after
`resolved_as_of`, `:40`):

```rust
    /// The generation a historical read resolved, when exactly one names it.
    /// A document request pins one partition and therefore one generation
    /// (see `query::document`'s `gen_id = N` predicate); a view request
    /// resolves one generation per partition, which no scalar names, so it
    /// stays `None`. A live read leaves it `None` too: the store answers
    /// that from the catalog, which the plan does not carry.
    pub resolved_generation: Option<i64>,
```

Add `resolved_generation: None,` to the literals at `compile.rs:894`,
`distinct.rs:108`, and `pool.rs:476`.

In `crates/geode-data/src/query/document.rs`, declare it beside
`resolved_as_of` (`:97`):

```rust
    let mut resolved_generation: Option<i64> = None;
```

and set it in the `Some(g)` arm, next to the existing
`resolved_as_of.insert(...)` (`:117`):

```rust
                    resolved_generation = Some(g.gen_id);
```

and add `resolved_generation,` to the returned literal (`:158`).

- [ ] **Step 8: Report the real generation from all four provenance arms**

In `crates/geode-data/src/query/read.rs`, replace the two view arms
(`:93-102`) with:

```rust
                        AsOf::Live => Freshness {
                            dataset: dataset.clone(),
                            as_of: catalog.dataset_as_of(dataset, &[])?.map(|t| t.to_rfc3339()),
                            generation: catalog.dataset_generation(dataset)?,
                        },
                        // A historical view read pins one generation per
                        // partition; no scalar names that, and a placeholder
                        // integer would be a lie now the field has a reader.
                        AsOf::At(_) => Freshness {
                            dataset: dataset.clone(),
                            as_of: compiled.resolved_as_of.get(dataset).map(|t| t.to_rfc3339()),
                            generation: None,
                        },
```

and the two document arms (`:115-133`) with:

```rust
                    AsOf::Live => Freshness {
                        dataset: params.dataset.clone(),
                        as_of: catalog
                            .live_source_time(
                                &params.dataset,
                                &join_key(&params.document_key),
                                None,
                            )?
                            .map(|t| t.to_rfc3339()),
                        generation: catalog.live_generation(
                            &params.dataset,
                            &join_key(&params.document_key),
                            None,
                        )?,
                    },
                    AsOf::At(_) => Freshness {
                        dataset: params.dataset.clone(),
                        as_of: compiled
                            .resolved_as_of
                            .get(&params.dataset)
                            .map(|t| t.to_rfc3339()),
                        // The document compiler pinned exactly one
                        // generation; report the one it read.
                        generation: compiled.resolved_generation,
                    },
```

`latest_gen_id` keeps its remaining caller at `catalog.rs:133`; do not remove it.

- [ ] **Step 9: Write the failing read tests**

Add to `crates/geode-data/src/query/read.rs`'s tests module:

```rust
    /// A corrected republish keeps its source time and takes a new
    /// generation. A reader holding unsent work over the older one can only
    /// tell them apart if provenance says so.
    #[test]
    fn a_republish_at_the_same_source_time_reports_a_different_generation() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("gen.duckdb")).unwrap();
        let ds = cvi_dataset();
        store.apply_schema(&ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        let publish = |value: f64| {
            publish_document(
                &store,
                &DocumentPublishRequest {
                    dataset: &ds,
                    source: "test",
                    rows: &cvi_doc("SPX.Z", [value; 6]),
                    source_time: ts("2026-09-12T14:00:00Z"),
                    received_at: ts("2026-09-12T14:00:00Z"),
                    bytes: 0,
                },
            )
            .unwrap();
        };
        publish(1.);
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds.clone());
        let config = Arc::new(ReadConfig {
            schema: Arc::new(schema),
            dimensions: DerivedDimensions::default(),
        });
        let params = DocumentParams {
            key: QueryKey(1),
            tag: 1,
            submitted: Instant::now(),
            dataset: ds.name.clone(),
            document_key: vec!["SPX.Z".into()],
            as_of: AsOf::Live,
        };
        let live = ReadQuery::document(Arc::clone(&config), params.clone());
        let reader = store.reader().unwrap();
        let first = live.run(&reader).unwrap();
        publish(2.);
        let second = live.run(&reader).unwrap();

        let (a, b) = (&first.provenance().datasets[0], &second.provenance().datasets[0]);
        assert_eq!(a.as_of, b.as_of, "the republish kept its source time");
        assert!(
            a.generation.is_some() && b.generation.is_some(),
            "a live document read names the generation it read"
        );
        assert_ne!(
            a.generation, b.generation,
            "a same-time republish must be distinguishable from its predecessor"
        );

        // A historical read of the same document reports the generation it
        // pinned, not a placeholder.
        let mut at = params;
        at.as_of = AsOf::At(ts("2026-09-12T14:30:00Z"));
        let historical = ReadQuery::document(config, at);
        assert_eq!(
            historical.run(&reader).unwrap().provenance().datasets[0].generation,
            b.generation,
            "as-of at a time after both publishes resolves the newer generation"
        );
    }
```

- [ ] **Step 10: Run the read tests**

Run: `cargo test -p geode-data a_republish_at_the_same_source_time`
Expected: PASS. If `DocumentParams` does not derive `Clone`, clone the fields
explicitly rather than adding a derive.

- [ ] **Step 11: Whole-workspace gate**

Run, and do not proceed until all three are clean:
```bash
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: the pre-existing `provenance: resolved vs requested time` harness
entry at `scripts/mutation-check.sh:609` now has a stale anchor (it quotes
`generation: 0,`). Leave it; Task 4 re-anchors it. Note in the report whether
`cargo fmt` reflowed the `live_source_time` call at `read.rs`, because the
harness entry at `scripts/mutation-check.sh:9529` anchors on its exact
multi-line shape.

- [ ] **Step 12: Commit**

```bash
git add -A
git commit -m "feat(data): provenance reports the generation actually read, per dataset

Freshness::generation was a database-global counter for live reads and a
literal 0 for historical ones, and nothing read it. It now names the
generation the answer came from, and None where no single generation
names the read."
```

---

### Task 2: A draft's base is (source time, generation)

**Files:**
- Modify: `crates/geode-marketdata/src/core/draft.rs` — `:20-40` (`DraftState`), `:134-160` (`Draft`), `:210-232` (`set`, `numeric_edit`), `:233-250` (`set_attr`), `:442-466` (`bump`), `:470-505` (`on_delivered`), `:700-724` (`rebase`, `badge`), `:752-760` (`to_toml`), `:840-842` (`from_toml`)
- Modify: `crates/geode-marketdata/src/core/matrix.rs` — `:96-105` (`MatrixModel`), `:185-240` (`build`)
- Modify: `crates/geode-marketdata/src/tile.rs` — `:232-240` (`Echo`), `:1345-1350` (`apply`), `:1358-1362`, `:1394-1400`, `:1475-1482`, `:1577-1607` (`echo_of`), `:1690-1700`, `:1948-1956`, `:2273-2289`, `:2749`, `:2863-2871`, `:2903-2904`, `:2931`, `:3466`, `:3919-3920`, `:4391-4397` (`source_time_of`)
- Test: the same three files' test modules

**Interfaces:**
- Consumes: `Freshness::generation: Option<i64>` (Task 1).
- Produces:
  - `geode_marketdata::core::draft::DocumentBase { as_of: String, generation: Option<i64> }` with `differs_from(&self, delivered: &DocumentBase) -> bool`
  - `geode_marketdata::core::matrix::base_of(snapshot: &Snapshot) -> Option<DocumentBase>` (`pub(crate)`)
  - `MatrixModel.base: Option<DocumentBase>` replaces `MatrixModel.source_time: Option<String>`
  - `Draft::base: Option<DocumentBase>`; `Draft::on_delivered(&mut self, delivered: &DocumentBase) -> bool`; `Draft::set`, `set_attr`, `insert_row`, `delete_row`, `bump` take `base: &DocumentBase`
  - `DraftState::Behind { newer: DocumentBase }`; `DraftBadge::Behind { newer: String }` **unchanged** (it feeds `local_hhmm`; `badge()` passes `newer.as_of.clone()`)

**What is wrong today.** `on_delivered` returns `false` when the delivered
`as_of` equals `Draft.base`, leaving the draft `Editing` — but `apply_snapshot`
rebuilds from the new snapshot and installs it regardless. `Draft::edits` is
keyed by document row and *model column*, and under an axis layout the model's
column order is the document's own node order, deliberately unsorted. So a
republish that keeps its source time and reorders or adds a node re-points every
edit onto a different node, paints it there, and `:upload` will send it. The
draft's label side map would catch this, but only `rebase` consults it.

- [ ] **Step 1: Write the failing `DocumentBase` tests**

Add to `crates/geode-marketdata/src/core/draft.rs`'s tests module:

```rust
    fn at(as_of: &str) -> DocumentBase {
        DocumentBase {
            as_of: as_of.to_string(),
            generation: None,
        }
    }

    fn at_gen(as_of: &str, generation: i64) -> DocumentBase {
        DocumentBase {
            as_of: as_of.to_string(),
            generation: Some(generation),
        }
    }

    #[test]
    fn a_same_time_republish_is_a_different_generation_when_both_ids_are_known() {
        // The defect this branch closes: identical source times, different
        // generations. Position-keyed edits must not be re-pointed silently.
        assert!(at_gen(BASE, 7).differs_from(&at_gen(BASE, 8)));
        assert!(!at_gen(BASE, 7).differs_from(&at_gen(BASE, 7)));
        // A different time always differs, generations or not.
        assert!(at_gen(BASE, 7).differs_from(&at_gen(NEWER, 7)));
        assert!(at(BASE).differs_from(&at(NEWER)));
        // An unknown generation is not evidence of movement: a restored
        // draft (whose edits are unresolved anyway) and a historical view
        // read fall back to the source time, the behaviour before the pair.
        assert!(!at(BASE).differs_from(&at_gen(BASE, 9)));
        assert!(!at_gen(BASE, 9).differs_from(&at(BASE)));
    }

    #[test]
    fn on_delivered_goes_behind_on_a_same_time_republish() {
        let mut draft = Draft::default();
        draft.set((0, 0), pair("T1", "-20"), Value::F64(1.0), &at_gen(BASE, 7));
        assert!(
            !draft.on_delivered(&at_gen(BASE, 7)),
            "the same generation redelivered changes nothing"
        );
        assert!(
            draft.on_delivered(&at_gen(BASE, 8)),
            "a republish at the same source time moved the document"
        );
        assert_eq!(
            draft.state,
            DraftState::Behind {
                newer: at_gen(BASE, 8)
            }
        );
        assert!(
            draft.on_delivered(&at_gen(BASE, 7)),
            "the edits' own generation coming back restores Editing"
        );
        assert_eq!(draft.state, DraftState::Editing);
    }
```

- [ ] **Step 2: Run them to confirm they fail**

Run: `cargo test -p geode-marketdata a_same_time_republish_is_a_different_generation`
Expected: FAIL — `cannot find type DocumentBase`.

- [ ] **Step 3: Add `DocumentBase`**

In `crates/geode-marketdata/src/core/draft.rs`, immediately before
`pub enum DraftState`:

```rust
/// The document generation an open draft's edits were made against: the
/// document's source time, and the store generation that delivered it when
/// one was reported.
///
/// Source time alone is not an identity. A corrected republish keeps its
/// source time, and `Draft::edits` is keyed by grid position — so a same-time
/// republish that reorders or adds a node would re-point every edit onto a
/// different node, paint it there, and `:upload` would send it. The
/// generation is what tells the two apart.
///
/// `generation` is `None` when the read could not name one: a draft restored
/// from a session file (whose cell edits are unresolved and require a rebase
/// before they can paint on any cell at all), or a historical view read,
/// whose era pins one generation per partition. An unknown generation is not
/// evidence of movement, so identity falls back to the source time — the
/// behaviour before this pair existed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentBase {
    /// RFC 3339.
    pub as_of: String,
    pub generation: Option<i64>,
}

impl DocumentBase {
    /// Whether `delivered` is a different document generation from this one.
    ///
    /// Differing source times always differ. Equal times differ only when
    /// both generations are known and disagree; see the type's own note on
    /// why an unknown generation cannot prove movement.
    pub fn differs_from(&self, delivered: &DocumentBase) -> bool {
        self.as_of != delivered.as_of
            || matches!(
                (self.generation, delivered.generation),
                (Some(mine), Some(theirs)) if mine != theirs
            )
    }
}
```

Export it from `crates/geode-marketdata/src/core/mod.rs:16`'s `pub use draft::{...}` list.

- [ ] **Step 4: Carry the pair through `Draft`**

In the same file:

1. `DraftState::Behind { newer: DocumentBase }` — update the variant and extend
   its doc comment's last sentence to: *"The edits' own base generation coming
   back (an as-of round trip, or a republish reverted upstream) returns the
   draft to `Editing` — see [`Draft::on_delivered`]. Identity is the pair, so a
   corrected republish at the same source time reaches this state too."*
2. `Draft::base: Option<DocumentBase>`, doc comment: *"The generation the
   edits were made against. Edit operations set it when the draft is empty or
   has no base; restoration can omit the generation half."*
3. Every door taking `base: &str` (`set`, `set_attr`, `insert_row`,
   `delete_row`, `bump`) takes `base: &DocumentBase`, and each
   `self.base = Some(base.to_string())` becomes `self.base = Some(base.clone())`.
4. `on_delivered`:

```rust
    /// Process a delivered generation and report whether state changed.
    /// An editing draft becomes `Behind` when the delivered generation differs
    /// from the base; a behind draft returns to `Editing` when its own base is
    /// delivered again. Clean and sent drafts are unchanged; the tile checks
    /// sent echoes.
    ///
    /// Identity is [`DocumentBase`]: source time *and*, when both are known,
    /// generation. A corrected republish at the same source time therefore
    /// reaches `Behind`, where before it was invisible and could re-point
    /// position-keyed edits onto other nodes. When either generation is
    /// unknown the comparison falls back to source time alone.
    pub fn on_delivered(&mut self, delivered: &DocumentBase) -> bool {
        match &self.state {
            DraftState::Editing
                if self
                    .base
                    .as_ref()
                    .is_none_or(|base| base.differs_from(delivered)) =>
            {
                self.state = DraftState::Behind {
                    newer: delivered.clone(),
                };
                true
            }
            // Returning to the base generation restores editing without moving
            // edits. Check this before updating the pending delivery.
            DraftState::Behind { .. }
                if self
                    .base
                    .as_ref()
                    .is_some_and(|base| !base.differs_from(delivered)) =>
            {
                self.state = DraftState::Editing;
                true
            }
            // Keep the badge on the latest delivered generation, which may be
            // historical when the user changes as-of.
            DraftState::Behind { newer } if newer != delivered => {
                self.state = DraftState::Behind {
                    newer: delivered.clone(),
                };
                true
            }
            _ => false,
        }
    }
```

5. `badge()`: `DraftState::Behind { newer } => DraftBadge::Behind { newer: newer.as_of.clone() }`.
   `DraftBadge` itself does not change.
6. `rebase`: `self.base = model_of_newer.base.clone();`
7. `to_toml`, after the existing `base` insert:

```rust
        if let Some(base) = &self.base {
            table.insert("base".into(), toml::Value::String(base.as_of.clone()));
            // Only when known. A session file written before generations
            // were carried, and a restored draft, simply have no generation
            // — `DocumentBase::differs_from` treats that as unknown, not as
            // "unchanged".
            if let Some(generation) = base.generation {
                table.insert("base_generation".into(), toml::Value::Integer(generation));
            }
        }
```

8. `from_toml`:

```rust
        let base = t
            .get("base")
            .and_then(|v| v.as_str())
            .map(|as_of| DocumentBase {
                as_of: as_of.to_string(),
                generation: t.get("base_generation").and_then(|v| v.as_integer()),
            });
```

- [ ] **Step 5: Convert the draft tests mechanically**

```bash
cd crates/geode-marketdata/src/core
perl -0pi -e 's/, BASE\)/, &at(BASE))/g; s/, NEWER\)/, &at(NEWER))/g' draft.rs
perl -0pi -e 's/on_delivered\(BASE\)/on_delivered(&at(BASE))/g;
              s/on_delivered\(NEWER\)/on_delivered(&at(NEWER))/g;
              s/on_delivered\(older\)/on_delivered(\&at(older))/g;
              s/on_delivered\("t9"\)/on_delivered(&at("t9"))/g' draft.rs
cd - >/dev/null
cargo test -p geode-marketdata --lib core::draft 2>&1 | tail -40
```

Fix the residue by hand — the perl passes are a starting point, not a
guarantee. `DraftState::Behind { newer: NEWER.to_string() }` literals in tests
become `DraftState::Behind { newer: at(NEWER) }`.

Do **not** add `impl From<&str> for DocumentBase`. It would let production code
pass a bare time and drop the generation silently, which is the drift this task
exists to remove.

- [ ] **Step 6: Run the draft tests**

Run: `cargo test -p geode-marketdata --lib core::draft`
Expected: PASS, including both new tests.

- [ ] **Step 7: Commit**

```bash
git add crates/geode-marketdata/src/core/draft.rs crates/geode-marketdata/src/core/mod.rs
git commit -m "feat(marketdata): a draft's base is source time and generation

A corrected republish keeps its source time, so on_delivered saw no
change and apply_snapshot re-pointed position-keyed edits onto whatever
node now held that column. The base is now the pair."
```

- [ ] **Step 8: One writer for a snapshot's base**

In `crates/geode-marketdata/src/core/matrix.rs`:

```rust
/// A delivered snapshot's document generation, from the first provenance
/// dataset. `None` when that dataset has never loaded, in which case there
/// is nothing for a draft to be based on.
///
/// The one reader of provenance in this crate: the tile and the model must
/// never derive a base two different ways.
pub(crate) fn base_of(snapshot: &Snapshot) -> Option<DocumentBase> {
    let freshness = snapshot.provenance().datasets.first()?;
    Some(DocumentBase {
        as_of: freshness.as_of.clone()?,
        generation: freshness.generation,
    })
}
```

Replace `MatrixModel.source_time` (`:100-102`) with:

```rust
    /// The document generation this model was built from, from the first
    /// provenance dataset. Draft identity compares the whole pair: a
    /// corrected republish keeps its source time and changes only the
    /// generation.
    pub base: Option<DocumentBase>,
```

In `build` (`:192`), replace the inline provenance read with
`let base = base_of(snapshot);` and both struct literals' `source_time` with
`base`.

- [ ] **Step 9: Wire the tile onto the pair**

In `crates/geode-marketdata/src/tile.rs`:

1. Delete `fn source_time_of` (`:4391-4397`) and import `core::matrix::base_of`.
   Replace each call: `source_time_of(&s)` → `base_of(&s)`.
2. `edit_base` and `attr_edit_base` return `Result<DocumentBase, String>` via
   `self.model.base.clone().unwrap_or_default()`. A default `DocumentBase` is
   the empty-string base those methods already produced, now with an unknown
   generation.
3. `:2903` `let base = self.model.source_time.clone().unwrap_or_default();` →
   `let base = self.model.base.clone().unwrap_or_default();`
4. `:1479` `.filter(|s| draft.base.is_some() && source_time_of(s) == draft.base)`
   → `.filter(|s| draft.base.is_some() && base_of(s) == draft.base)`
5. `:1697` `if source_time_of(&base) != draft.base {` →
   `if base_of(&base) != draft.base {`
6. `:1394` `let as_of = source_time_of(&snapshot);` → `let base = base_of(&snapshot);`,
   and `let mut moved = base.as_ref().is_some_and(|b| draft.on_delivered(b));`.
   The `echo_of` call on the next line passes `base.as_ref()`. The
   `UpdatePolicy::Replace` arm's `when` becomes
   `base.as_ref().map(|b| local_hhmm(&b.as_of, self.clock)).unwrap_or_default()`.
   `withdraw_upload_if_moved`'s `painted` parameter becomes
   `Option<DocumentBase>`, since both its callers now hand it `base_of(&s)`.
7. `Echo::Differs { newer: DocumentBase, text: SharedString }`, and `echo_of`
   takes `delivered: Option<&DocumentBase>` instead of `as_of: Option<&str>`.
   Its two comparisons become:
   - `if draft.base.as_ref().is_some_and(|b| !b.differs_from(delivered)) { return Ok(EchoStep::None); }`
   - `if let Some(held @ Echo::Differs { newer, .. }) = &self.echo && newer == delivered { ... }`

   The memo must key on the pair for the same reason the draft does: keyed on
   time alone, a corrected republish would reuse the previous republish's
   comparison result.
8. `:1948-1956` `self.source_at` reads `self.model.base.as_ref().map(|b| b.as_of.as_str())`
   before the RFC-3339 parse.
9. Spec §4.2's "a notice naming it": under the default `Hold` policy a
   same-time republish would show `update 14:05` over a base also stamped
   14:05, leaving the trader comparing a timestamp with itself. In
   `apply_snapshot`, after the policy `match` closes and before `retained` is
   computed (`:1468`), add:

```rust
        // A republish at the SAME source time is invisible in the `update
        // HH:MM` badge, because the base carries that time too. Say so.
        // Only when no policy notice already speaks for this delivery: a
        // `Replace` disclosure of lost work outranks this one.
        if moved
            && draft.is_behind()
            && notice.is_none()
            && let Some(delivered) = &base
            && draft
                .base
                .as_ref()
                .is_some_and(|held| held.as_of == delivered.as_of)
        {
            notice = Some(
                format!(
                    "republished at {} — :rebase or :revert",
                    local_hhmm(&delivered.as_of, self.clock)
                )
                .into(),
            );
        }
```

- [ ] **Step 10: Write the failing tile test**

In `crates/geode-marketdata/src/tile.rs`'s test module, beside the existing
`provenance` helper (`:4640`), add a generation-aware sibling and leave
`provenance` delegating to it so no existing test changes:

```rust
    fn provenance(as_of: &str) -> Provenance {
        provenance_gen(as_of, 7)
    }

    fn provenance_gen(as_of: &str, generation: i64) -> Provenance {
        Provenance {
            datasets: vec![Freshness {
                dataset: "cvi_params".into(),
                as_of: Some(as_of.into()),
                generation: Some(generation),
            }],
            as_of_request: None,
        }
    }
```

Then the behaviour test, modelled exactly on
`a_newer_generation_under_a_draft_goes_behind_and_keeps_painting_the_base`
(`:8664`), which is the canonical Behind route. `document_with` already takes a
caller-supplied provenance, so no new snapshot fixture is needed:

```rust
    /// A corrected republish keeps its source time and takes a new store
    /// generation. Before the base was a pair, `on_delivered` saw no change,
    /// `apply_snapshot` installed the new grid anyway, and this edit — keyed
    /// by grid position — landed on whatever node now held that column.
    #[gpui::test]
    fn a_republish_at_the_same_source_time_holds_the_draft_instead_of_repointing_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_with(&TERMS, &NODES, provenance_gen(BASE, 7))),
        );

        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);

        // The SAME source time, a new generation, and a document whose
        // columns have moved: one term dropped, so every axis column shifts.
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_with(
                &["2026-11-20"],
                &NODES,
                provenance_gen(BASE, 8),
            )),
        );

        let (state, rows, cell, notice) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().state.clone(),
                t.model().rows.len(),
                t.model().rows[0].cells[0].clone(),
                t.notice_text(),
            )
        });
        assert!(
            matches!(
                state,
                DraftState::Behind { ref newer }
                    if newer.as_of == BASE && newer.generation == Some(8)
            ),
            "a same-time republish must be Behind, got {state:?}"
        );
        assert_eq!(rows, 2, "still painting the base generation's two terms");
        assert_eq!(cell.text.to_string(), "0.50", "the edit is still its own");
        assert!(cell.edited);
        assert!(
            notice.is_some_and(|n| n.contains("republished")),
            "the badge's time is the base's own, so the notice must say what moved"
        );
    }
```

`notice_text()` is the test accessor the neighbouring tests use for
`self.notice`; if it is named differently, find it with
`grep -n 'fn notice' crates/geode-marketdata/src/tile.rs` and use that name
rather than adding another.

- [ ] **Step 11: Run it, then the crate**

```bash
cargo test -p geode-marketdata a_republish_at_the_same_source_time_holds
cargo test -p geode-marketdata
cargo check -p geode-shell --features test-support --all-targets
```
Expected: PASS. The new test must fail before Step 9's wiring is in place —
if it passes against the old code, it is not testing the production route.

- [ ] **Step 12: Workspace gate and commit**

```bash
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git add -A
git commit -m "feat(marketdata): a same-time republish puts an open draft Behind

The model and the tile now derive one DocumentBase through core::matrix::base_of,
and the echo memo keys on the pair too."
```

---

### Task 3: An I64 cell keeps its type

**Files:**
- Modify: `crates/geode-marketdata/src/core/draft.rs` — `:226-232` (`numeric_edit`), `:993-1005` (`bumped`), `:1029-1053` (`parse_cell`), `:1055-1073` (`parse_attr`), `:442-466` (`Draft::bump`)
- Modify: `crates/geode-marketdata/src/tile.rs` — `:2671-2692` (`commit_cell_edit`), `:3483-3493` (the `numeric_value` closure), `:3509-3594` (`bump`)
- Test: both files' test modules

**Interfaces:**
- Consumes: `Draft::bump`'s `base: &DocumentBase` (Task 2).
- Produces:
  - `parse_cell(text: &str, ty: ColumnType) -> Result<Value, String>` (was `Result<f64, String>`)
  - `bumped(current: &Value, delta: f64, ty: ColumnType, column: &str) -> Result<Value, String>`
  - `Draft::numeric_edit(&self, cell: (usize, usize)) -> Option<&Value>`
  - `Draft::bump(cells: impl Iterator<Item = ((usize, usize), (String, String), Value, ColumnType)>, delta: f64, base: &DocumentBase) -> Result<usize, String>`

**What is wrong today.** `parse_cell`'s I64 arm is
`.parse::<i64>().map(|v| v as f64)`, and `commit_cell_edit` then does
`Value::I64(parsed as i64)`. Eight lines below `parse_cell`, `parse_attr`'s I64
arm is `.parse::<i64>().map(Value::I64)` under a comment stating that a round
trip through a double "loses every integer above 2^53, silently". The same typed
value is therefore exact in an attribute and truncated in a cell. `bumped` takes
and returns `f64` for the same reason, and `numeric_edit` widens a stored
`Value::I64` on the way in.

**One function, not two.** The spec says `parse_cell` "gains a `Value`-returning
sibling". Do not add a sibling: change `parse_cell` itself and have `parse_attr`
delegate both numeric arms to it. Two functions parsing the same integer
differently is exactly the drift that produced this defect; one is the fix.
(Recorded as a ruling in the branch's ledger.)

**No new 2^53 refusal.** The spec's failure semantics mention refusing a value
too large to represent. With a true `i64` parse there is nothing to refuse —
the value is exact. The refusals stay what they are: non-numeric text, a
fractional value in an integer column, a non-finite float.

- [ ] **Step 1: Write the failing tests**

In `crates/geode-marketdata/src/core/draft.rs`'s tests module, replace the body
of `parse_cell_reads_f64_and_i64_and_names_the_text_it_refused` with:

```rust
        assert_eq!(parse_cell(" 0.25 ", ColumnType::F64), Ok(Value::F64(0.25)));
        assert_eq!(parse_cell("-3", ColumnType::F64), Ok(Value::F64(-3.0)));
        assert_eq!(parse_cell("7", ColumnType::I64), Ok(Value::I64(7)));
        // 2^53 + 1. Through an f64 this is 9007199254740992 — the whole
        // reason `parse_attr` parses integers directly.
        assert_eq!(
            parse_cell("9007199254740993", ColumnType::I64),
            Ok(Value::I64(9007199254740993))
        );

        let err = parse_cell("0.5", ColumnType::I64).expect_err("a whole number only");
        assert!(err.contains("whole number"), "{err}");
        let err = parse_cell("abc", ColumnType::F64).expect_err("not a number");
        assert!(err.contains("abc"), "{err}");
        let err = parse_cell("", ColumnType::F64).expect_err("nothing is not a number");
        assert!(!err.is_empty());
```

Keep the rest of that test's assertions, adjusting them to `Value`.

Add:

```rust
    #[test]
    fn a_bump_of_an_integer_column_stays_an_integer_above_2_pow_53() {
        // The f64 path turned 9007199254740993 + 1 into ...92, losing both
        // the bump and the original value.
        assert_eq!(
            bumped(
                &Value::I64(9007199254740993),
                1.0,
                ColumnType::I64,
                "lots"
            ),
            Ok(Value::I64(9007199254740994))
        );
        assert_eq!(
            bumped(&Value::F64(0.25), 0.5, ColumnType::F64, "vol"),
            Ok(Value::F64(0.75))
        );
        // A fractional delta on an integer column is still refused.
        let err = bumped(&Value::I64(3), 0.5, ColumnType::I64, "lots")
            .expect_err("a fractional delta");
        assert!(err.contains("whole numbers"), "{err}");
        // A fractional value already sitting in an integer column is
        // refused rather than truncated into one.
        let err = bumped(&Value::F64(1.5), 1.0, ColumnType::I64, "lots")
            .expect_err("a fractional current value");
        assert!(err.contains("fractional"), "{err}");
        // Overflow refuses rather than wrapping or saturating.
        let err = bumped(&Value::I64(i64::MAX), 1.0, ColumnType::I64, "lots")
            .expect_err("an overflowing bump");
        assert!(err.contains("too large"), "{err}");
        // A text value in a numeric column is not bumpable.
        let err = bumped(&Value::Utf8("x".into()), 1.0, ColumnType::F64, "note")
            .expect_err("not a number");
        assert!(!err.is_empty());
    }
```

- [ ] **Step 2: Run them to confirm they fail**

Run: `cargo test -p geode-marketdata a_bump_of_an_integer_column_stays_an_integer`
Expected: FAIL — argument type mismatch on `bumped`.

- [ ] **Step 3: Retype `parse_cell` and `parse_attr`**

```rust
/// Parse a numeric cell at its declared type, refusing nonnumeric types and
/// nonfinite floating values. An integer column parses as `i64` and STAYS
/// one: a round trip through a double loses every integer above 2^53,
/// silently. Callers dispatch text, choice, and date cells separately.
/// Errors retain the refused input.
pub fn parse_cell(text: &str, ty: ColumnType) -> Result<Value, String> {
    let trimmed = text.trim();
    match ty {
        ColumnType::F64 => {
            let value: f64 = trimmed
                .parse()
                .map_err(|_| format!("'{text}' is not a number"))?;
            if !value.is_finite() {
                return Err(format!("'{text}' is not a finite number"));
            }
            Ok(Value::F64(value))
        }
        ColumnType::I64 => trimmed
            .parse::<i64>()
            .map(Value::I64)
            .map_err(|_| format!("'{text}' is not a whole number")),
        ColumnType::Utf8 | ColumnType::Date | ColumnType::Timestamp | ColumnType::Bool => Err(
            format!("'{text}' cannot be entered here — not a numeric cell"),
        ),
    }
}
```

In `parse_attr`, both numeric arms become one delegation — the duplicate
integer parse and its comment go away, because `parse_cell` now carries the
rule:

```rust
        ColumnType::F64 | ColumnType::I64 => parse_cell(text, ty),
```

- [ ] **Step 4: Retype `bumped`, `numeric_edit`, and `Draft::bump`**

```rust
/// Add `delta` to `current` at the column's declared type.
///
/// An integer column does integer arithmetic: neither the value nor the
/// result passes through an `f64`, so a holding above 2^53 survives a bump.
/// A fractional delta, a fractional value already sitting in an integer
/// column, a non-numeric value, and an overflowing result are all refused
/// by name — a plausible wrong quantity is worse than a refusal.
pub fn bumped(
    current: &Value,
    delta: f64,
    ty: ColumnType,
    column: &str,
) -> Result<Value, String> {
    match ty {
        ColumnType::F64 => match current {
            Value::F64(v) => Ok(Value::F64(v + delta)),
            Value::I64(v) => Ok(Value::F64(*v as f64 + delta)),
            other => Err(format!("bump: {column} holds {other:?}, not a number")),
        },
        ColumnType::I64 if delta.fract() != 0.0 => {
            Err(format!("bump: {column} takes whole numbers"))
        }
        ColumnType::I64 => {
            let current = match current {
                Value::I64(v) => *v,
                // A whole-valued double in an integer column is the same
                // number; anything else would have to be truncated, and
                // `:bump` does not silently change a holding.
                Value::F64(v) if v.fract() == 0.0 && v.is_finite() => *v as i64,
                Value::F64(v) => {
                    return Err(format!("bump: {column} holds a fractional value ({v})"));
                }
                other => return Err(format!("bump: {column} holds {other:?}, not a number")),
            };
            // `delta as i64` is exact for every whole delta a trader can
            // type that an f64 represents exactly; beyond that the delta
            // was already imprecise when it was parsed.
            current
                .checked_add(delta as i64)
                .map(Value::I64)
                .ok_or_else(|| format!("bump: {column} would be too large"))
        }
        other => Err(format!("bump: {column} is not numeric ({other:?})")),
    }
}
```

`numeric_edit` returns the stored value rather than a widened double:

```rust
    /// Read an existing numeric edit; absent, date, and text edits return
    /// `None`. Callers use this before the painted document value so
    /// successive bumps compose, and the value keeps its type so an integer
    /// column's holding is never widened on the way through. The caller also
    /// filters eligible numeric columns; this method only reads.
    pub fn numeric_edit(&self, cell: (usize, usize)) -> Option<&Value> {
        match self.edits.get(&cell)? {
            value @ (Value::F64(_) | Value::I64(_)) => Some(value),
            Value::Utf8(_) | Value::Date(_) => None,
        }
    }
```

`Draft::bump`'s iterator item becomes `((usize, usize), (String, String), Value, ColumnType)`
and its loop `let value = bumped(&current, delta, ty, &labels.1)?;`.

- [ ] **Step 5: Retype the tile's two call paths**

In `crates/geode-marketdata/src/tile.rs`:

1. `commit_cell_edit`'s `Number` arm loses its re-wrap entirely:

```rust
                match parse_cell(text, ty) {
                    Ok(parsed) => parsed,
                    Err(e) => {
                        // Stay in insert mode, with the text as typed.
                        self.notice = Some(e.into());
                        return true;
                    }
                }
```

2. The `numeric_value` closure returns `Option<Value>`:

```rust
        let numeric_value = |state: RowState, cell: &Cell| -> Option<Value> {
            let edit = match state {
                RowState::Inserted => None,
                RowState::Document | RowState::Deleted => {
                    self.draft.numeric_edit(cell.cell_ref).cloned()
                }
            };
            edit.or(match &cell.value {
                Some(value @ (Value::F64(_) | Value::I64(_))) => Some(value.clone()),
                Some(Value::Utf8(_) | Value::Date(_)) | None => None,
            })
        };
```

3. `values`, `document`, and `inserted` carry `Value` instead of `f64`:
   `let values: Vec<((usize, usize), Value, ColumnType)>`,
   `let mut inserted: Vec<((String, String), Value, ColumnType)>`, and
   `BumpCell`'s value field likewise (find its definition with
   `grep -n 'BumpCell' crates/geode-marketdata/src/tile.rs`).
4. The three `bumped` call sites take a reference:
   `bumped(value, delta, *ty, &labels.1)?;` (validation, twice) and
   `let value = bumped(&value, delta, ty, &col_label)?;` (the inserted write).

- [ ] **Step 6: Write the failing tile test**

Add to `crates/geode-marketdata/src/tile.rs`'s test module, beside
`a_fractional_row_bump_on_a_mixed_inserted_row_writes_nothing` (`:7262`), which
already relies on the mixed F64/I64 fixture this needs. Column 1 of
`test_fixtures::MIXED` is `n`, an I64:

```rust
    /// An integer cell commits the exact integer that was typed. Through the
    /// old `parse_cell` → f64 → `as i64` path the value below silently became
    /// 9007199254740992, while the same text in an ATTRIBUTE was exact.
    #[gpui::test]
    fn a_typed_integer_above_2_pow_53_reaches_the_draft_exactly(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_spec(cx, &test_fixtures::MIXED, None);
        h.command(&mut vcx, "key SPX.Z").expect("a valid key");
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(test_fixtures::mixed_snapshot(&[("M1", 1.0, 2)])),
        );

        h.tile.update(&mut vcx, |t, cx| t.cursor_to(0, Some(1), cx));
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "9007199254740993");
        h.dispatch(&mut vcx, "commit", None);

        let values: Vec<Value> = h
            .tile
            .read_with(&vcx, |t, _| t.draft().edits.values().cloned().collect());
        assert_eq!(
            values,
            vec![Value::I64(9007199254740993)],
            "the typed integer, not its f64 rounding"
        );

        // And a whole-number bump of it stays exact.
        h.command(&mut vcx, "bump 1 col").expect("a whole delta");
        let values: Vec<Value> = h
            .tile
            .read_with(&vcx, |t, _| t.draft().edits.values().cloned().collect());
        assert_eq!(values, vec![Value::I64(9007199254740994)]);
    }
```

If `bump 1 col` on this fixture also touches the F64 column and so writes two
edits, assert on the `n` cell's own entry rather than the whole map — the
integer's exactness is the claim, not the edit count.

- [ ] **Step 7: Run the crate and the workspace**

```bash
cargo test -p geode-marketdata
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "fix(marketdata): an integer cell keeps its type from text to upload

parse_cell widened every I64 to f64 and commit_cell_edit narrowed it back,
so any holding above 2^53 was silently truncated — while parse_attr, eight
lines away, parsed the same value exactly. One function now, and bumped
does integer arithmetic."
```

---

### Task 4: Harness entries and documentation

**Files:**
- Modify: `scripts/mutation-check.sh`
- Modify: `docs/current/features.md:59-81`, `:138`
- Modify: `docs/current/data-path.md:292-300`, `:377-383`
- Modify: `crates/geode-marketdata/README.md:9-18`
- Modify: `crates/geode-data/README.md` (the freshness/generation bullets near `:95`)

**Interfaces:** consumes every signature the three earlier tasks produced.

**Preflight — ten entries this branch invalidates.** Each was found by asking
"which anchors *contain* any line the fix replaces", not "which anchors equal
it". Re-anchor every one, keeping the entry's name, comment, and covering test
unless the comment's claim itself has changed.

| Line | Entry | Broken by |
|---|---|---|
| ~609 | `provenance: resolved vs requested time` | Task 1 — the anchor quotes `generation: 0,` in both halves |
| ~9529 | `service: a live document's freshness is its own…` | Task 1 — check only; the anchor is the `live_source_time(…)` call, which should survive `cargo fmt` |
| ~11886 | `draft: on_delivered goes Behind only on a different as_of` | Task 2 |
| ~11910 | `to_toml_and_from_toml_round_trip_the_edits_by_label_and_the_base` | Task 2 — `from_toml`'s base line |
| ~12003 | (capture-groups guard) `if source_time_of(&base) != draft.base {` | Task 2 |
| ~12217 | `mdedit: a commit parses the typed text before writing it` | Task 3 — `match Ok::<f64, String>(0.0)` no longer type-checks |
| ~12522 | `final: the base generation redelivered leaves Behind` | Task 2 |
| ~12541 | (base retention) `.filter(|s| draft.base.is_some() && source_time_of(s) == draft.base),` | Task 2 |
| ~13727 | `mdattr: an I64 attribute parses exactly above 2^53` | Task 3 — the arm it mutates is gone; its rule now lives in `parse_cell` |
| ~15273 | `mdedit: an inserted-row bump checks every cell before writing any` | Task 3 — `bumped(*value, …)` |

- [ ] **Step 1: Validate every anchor before touching anything**

Run: `zsh scripts/mutation-check.sh --anchors-only`
Expected: non-zero exit, naming the stale anchors. Record the exact list in the
task report — it is the ground truth for this step, and the table above is only
a prediction.

- [ ] **Step 2: Re-anchor the ten entries**

Work from Step 1's output, not the table. For each, open the current source,
copy the replacement text exactly, and check by eye that the mutation still
breaks the behaviour the entry's name claims. Two need more than a re-quote:

- `mdattr: an I64 attribute parses exactly above 2^53` — the mutation target
  moves to `parse_cell`'s I64 arm and the entry is renamed
  `mdnum: an integer cell and attribute both parse exactly above 2^53`:

```
run_mutation "mdnum: an integer cell and attribute both parse exactly above 2^53" \
  crates/geode-marketdata/src/core/draft.rs \
  '        ColumnType::I64 => trimmed
            .parse::<i64>()
            .map(Value::I64)
            .map_err(|_| format!("'"'"'{text}'"'"' is not a whole number")),' \
  '        ColumnType::I64 => trimmed
            .parse::<f64>()
            .map(|v| Value::I64(v as i64))
            .map_err(|_| format!("'"'"'{text}'"'"' is not a whole number")),' \
  geode-marketdata parse_cell_reads_f64_and_i64_and_names_the_text_it_refused
```

- `mdedit: a commit parses the typed text before writing it` — the mutation
  becomes `match Ok::<Value, String>(Value::F64(0.0)) {`, and the comment keeps
  its reasoning unchanged.

- [ ] **Step 3: Add the six new entries**

Place each beside the entries it belongs with, with a comment saying what the
mutation lets through, and confirm by hand that the named test fails under it.

1. **`provenance: a live dataset's generation is its own, not the database's`** —
   `crates/geode-data/src/query/read.rs`, mutate
   `generation: catalog.dataset_generation(dataset)?,` to
   `generation: catalog.latest_gen_id()?.into(),`, caught by
   `a_republish_at_the_same_source_time_reports_a_different_generation`.
   (If that test reads only the document path, add the assertion it needs or
   name the test that does — do not commit an entry whose test does not fail.)
2. **`provenance: an as-of document reports the generation it pinned`** —
   `crates/geode-data/src/query/read.rs`, mutate
   `generation: compiled.resolved_generation,` to `generation: None,`.
3. **`catalog: a partition's live generation is its own`** —
   `crates/geode-data/src/store/catalog.rs`, mutate `live_generation`'s
   `where fg.dataset = ? and fg.batch = ?` to `where fg.dataset = ?` (with the
   parameter list adjusted so it still compiles), caught by
   `live_generation_is_the_partitions_newest_and_moves_on_a_same_time_republish`.
4. **`draft: identity is the generation pair, not the source time alone`** —
   `crates/geode-marketdata/src/core/draft.rs`, mutate `differs_from`'s body to
   `self.as_of != delivered.as_of`, caught by
   `a_same_time_republish_is_a_different_generation_when_both_ids_are_known`.
   This is the branch's headline guard.
5. **`draft: an unknown generation is not evidence of movement`** —
   the same function, mutate the `matches!` arm's `(Some(mine), Some(theirs))`
   to `(mine, theirs)` so an unknown generation counts as different, caught by
   the same test's last two assertions. Without this, every restored draft goes
   `Behind` on its first delivery.
6. **`mdnum: a bump of an integer column does integer arithmetic`** —
   `crates/geode-marketdata/src/core/draft.rs`, mutate `bumped`'s I64 success
   path to `Ok(Value::I64((current as f64 + delta) as i64))`, caught by
   `a_bump_of_an_integer_column_stays_an_integer_above_2_pow_53`.

- [ ] **Step 4: Run the new and re-anchored entries by name**

```bash
zsh scripts/mutation-check.sh --anchors-only
zsh scripts/mutation-check.sh "provenance:"
zsh scripts/mutation-check.sh "catalog: a partition"
zsh scripts/mutation-check.sh "draft:"
zsh scripts/mutation-check.sh "mdnum:"
zsh scripts/mutation-check.sh "mdedit:"
zsh scripts/mutation-check.sh "mdattr:"
zsh scripts/mutation-check.sh "final: the base generation"
```

Expected: `--anchors-only` exits 0; every named run reports `caught` for every
entry. Run these in the **foreground** — do not background them, and never pass
`--changed`, which selects unrelated entries and edits tracked files outside
this branch's scope. Commit the branch's work before running any of them: the
harness edits files in place and restores them afterwards.

- [ ] **Step 5: Update the current guides**

`docs/current/features.md` — the paragraph at `:59` and the one at `:79`. The
`:79` claim is the defect this branch closes and must be rewritten, not
softened:

> Edits live in a `Draft` over a base identified by the document's source time
> **and the store generation that delivered it.** …
>
> A base is a generation, not an instant. A historical delivery puts the draft
> Behind, and so does a corrected republish at the same source time: the
> generation differs even when the time does not. Returning to the base
> generation restores Editing. Where a read cannot name a generation — a draft
> restored from a session file, or a historical view read, which pins one
> generation per partition — the comparison falls back to source time alone;
> a restored draft's cell edits are unresolved until a rebase in any case.

`:138` — "compares the next delivered generation with a different source time"
becomes "compares the next delivered generation, once it differs from the base
generation".

`docs/current/data-path.md` — extend the freshness section (`:377`) with:

> Provenance also reports the generation each dataset was read from. A live
> read names the newest generation of the partition (a document) or of the
> dataset (a view); a historical document read names the generation it pinned.
> A historical view read reports no generation, because its era resolves one
> generation per partition and no single ID names the answer. An absent
> generation means unknown, never unchanged: a reader deciding whether the
> data under it moved falls back to source time and must treat that as the
> weaker test it is.

`crates/geode-marketdata/README.md:14` — "A later document generation is
compared separately" becomes "A later document generation — a different source
time, or the same time at a different store generation — is compared
separately".

`crates/geode-data/README.md` — add to the generation bullets near `:95` that
the catalog exposes a partition's and a dataset's live generation for
provenance, and that `latest_gen_id` remains an internal sequence helper and is
not a freshness answer.

- [ ] **Step 6: Final gate**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo bench --workspace --no-run
cargo check -p geode-shell --features test-support --all-targets
zsh scripts/mutation-check.sh --anchors-only
```

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "test(marketdata): harness entries for generation identity, and docs

Six new entries and ten re-anchored. features.md's claim that a same-time
republish is indistinguishable to the transition logic is no longer true."
```

---

## Rulings recorded while planning

Carry these into the branch ledger; each deviates from the spec's letter or
settles something the spec left open.

1. **A historical *view* read reports no generation** (spec §4.2 says "the
   as-of arms report the resolved generation rather than zero"). `era_for`
   resolves one generation per partition and `CompiledQuery` carries only
   `resolved_as_of: BTreeMap<String, DateTime<Utc>>`; a scalar cannot name a
   set. The document as-of arm — the one that matters for drafts, since uploads
   are live-only — *does* report its pinned generation, which is in hand at
   `document.rs:117`. Cost if wrong: a historical view's provenance carries
   less than it could, and any future reader must handle `None` (which it must
   anyway, for a dataset that has never loaded).
2. **`Freshness::generation` becomes `Option<i64>`, not a sentinel.** `0` was
   safe only while nothing read the field. Cost if wrong: thirteen construction
   sites and one field type change.
3. **`parse_cell` changes rather than gaining a sibling** (spec §4.2 says
   "gains a `Value`-returning sibling"). Two functions parsing the same integer
   differently is the drift that caused the defect. Cost if wrong: every
   `parse_cell` caller is touched — there are two outside its own tests.
4. **No new 2^53 refusal.** Spec §4.3's "refused at parse rather than silently
   truncated" is satisfied by parsing as `i64`, which is exact; there is
   nothing left to refuse. The refusals stay non-numeric text, a fractional
   value in an integer column, and overflow.
5. **`DraftState::Behind` carries the pair; `DraftBadge::Behind` keeps its
   `String`.** The badge renders `update HH:MM` through `local_hhmm`, and a
   same-time republish correctly reads as an update at that source time. No new
   badge state, per §4.3's "no third behaviour is invented". Cost if wrong: a
   trader seeing `update 14:05` over a base also stamped 14:05 must read the
   notice to know which it is.
6. **`Echo::Differs` keys on the pair too.** The memo exists to avoid
   recomparing the same delivered generation; keyed on time alone it would
   reuse a previous republish's verdict for a new one — the same defect one
   field over.
7. **One `base_of`, in `core::matrix`.** The tile and the model derive the base
   from provenance identically today, in two places. Consolidating removes a
   drift site rather than adding one.
