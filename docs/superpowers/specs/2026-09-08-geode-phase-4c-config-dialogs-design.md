# Geode Phase 4c — Config Dialogs Design

Supersedes §5 of
`docs/superpowers/specs/2026-09-06-geode-phase-4-frame-features-design.md`
("Phase 4c — the config editor") in full. That section is left in place
as the historical record; where the two disagree, this document wins.

## 1. Scope

### 1.1 Why the config editor was dropped

Old 4c put a syntax-highlighted TOML buffer in a tile: `EditorState`
with `.language("toml")`, `toml_edit` diagnostics as squiggles, `ctrl+s`
to save. It was rejected on user direction before implementation
started: editing TOML in-app is the wrong shape. Geode would be
competing with the trader's real editor at everything an editor is good
at, while giving up the thing an application can do that a text buffer
cannot — offer only the choices that exist, in the vocabulary of the
domain, validated by construction.

What replaces it is several purpose-built UIs, one per config function,
in the mould of a modern editor's separate "Keyboard Shortcuts" and
"User Settings" screens rather than one buffer over all of them.

Two of those already exist and set the pattern: `shell::settings_view`
and `shell::keybindings_view` — pure, unit-testable row state plus a
`dialog::ModalKeyHandler` with first refusal on every keystroke, over a
flat filterable row list. Phase 4c extends that pattern to four more
config domains and consolidates the write path they all need.

### 1.2 What Phase 4c delivers

**The scaffold** (`geode-shell`, new module `shell::objectdialog`): one
two-stage modal — browse a domain's named objects, edit one object's
fields — with create, delete, revert-to-desk, override marking and drift
detection implemented once. Four thin per-domain adapters supply what
differs.

**Four dialogs**: Views & columns, Sources, Scopes, Groupings. Full
create/edit/delete in each, writing the user layer.

**A read-only schema inspector** for `datasets` and `dimensions`, which
the other dialogs read to offer valid choices.

**The write door** (`geode-shell`, new module `config_write`): one
atomic write and one `toml_edit` read-modify-write, replacing three
`write_atomic` implementations (`theme.rs:524`, `keymap_edit.rs:253`,
`session.rs:972`) and backing the six existing persist paths
(`theme::persist_to_user_config`, `fontsize::persist_to_user_config`,
`vimfind::persist_to_user_config`, `frame::persist_slot_to_user_config`,
`frame::persist_scope_to_user_config`, `keymap_edit`) as well as the
four dialogs.

**`ViewPresentationSpec`** (`geode-core`) and the user-layer
`view_presentation.toml` of old 4c §5.6, unchanged in shape, now written
by the Views dialog through a per-field destination rather than by a
`:cols save` command.

**The reload prompt and the swappable `DataHandle`** of old 4c §5.7,
unchanged, now triggered by a Sources dialog write rather than a text
buffer's `ctrl+s`.

**`Diagnostic.path`** of old 4c §5.3, kept but repurposed: with no text
buffer there are no spans, and a reader's key path instead attaches a
diagnostic to the field row it belongs to.

### 1.3 Done state

Phase 4c is done when, in `geode --demo`:

- `config::views` from the palette lists the demo's views with their
  owning layer; opening `tree` shows its dataset and its ordered column
  list; moving a column, hiding one and setting a width persists to
  `view_presentation.toml`, and the blotter reflects all three without a
  restart;
- changing `tree`'s dataset writes a whole-object override into the
  user layer's `views.toml`, and the row is marked `overridden`;
- `Revert to desk` on that row deletes the user copy and the blotter
  returns to the desk's view;
- editing the demo layer's `tree` underneath an override marks the row
  `drifted`;
- `config::sources` creates a second source, prompts "reload data
  now?", and the blotters requery against it without a restart;
- `config::scopes` lists saved scopes, loads one into the frame, saves
  the frame's current scope over it, and deletes another;
- `config::groupings` reorders slot 3's dimensions and `ctrl+3`
  regroups by the new order;
- `config::schema` shows each dataset's grains, columns, types and
  `categorical`/`textual`/`dimension` flags with provenance, and is
  read-only;
- a field whose value the doc's own reader rejects shows its diagnostic
  on that field's row, before any write;
- no dialog opens a file itself — every write goes through
  `config_write`, off the render thread;
- and every behaviour above has a mutation entry.

### 1.4 Explicitly not in Phase 4c

- **Editing TOML text anywhere in-app.** No editor tile, no `:edit`, no
  `:copy user`. The `tree-sitter-toml` feature old 4c §5.1 required is
  not turned on.
- **A diff view for drift.** The marker plus `Revert to desk` is the
  whole affordance; showing *what* changed is its own surface and has no
  user story yet.
- **Editing a scope's values in the Scopes dialog** (§8.4). The picker
  and `:scope save` already author scope contents; duplicating that
  editor would diverge from it.
- **Editing `datasets` or `dimensions`.** They are the desk's schema
  contract, a change is restart-required, and the inspector is read-only
  (§9).
- **New default key bindings to *open* a dialog.** Every dialog is
  palette-only, as `keybindings::open` already is (§10). The verbs
  *inside* a dialog are letters, per the interaction model.
- **The workspace scope layer**, vim-modal editing, multi-window,
  scenario datasets, the sidecar split and as-of diffing — all still out,
  per Phase 4 §1.3.

## 2. Amendments to earlier designs

1. **Phase 4 §5 is superseded**, as stated above.
2. **`SourceSpec::from_doc` moves from `geode-data` to `geode-core`.**
   `geode-shell` may never depend on `geode-data` (CLAUDE.md), so a
   Sources dialog in the shell cannot otherwise run the reader that
   validates what it writes. The reader already depends only on
   `MergedDoc` and `SchemaSpec`, both in `geode-core`; discovery,
   ingest and `SourceSpec`'s runtime use stay in `geode-data`, which
   re-exports the type so its own call sites are unchanged.
3. **`ShellEvent` gains `ReloadRejected(Vec<Diagnostic>)`** (old 4c
   §5.4), so a write the merge rejects can be reported by the dialog
   that made it instead of only reaching stderr.

## 3. The scaffold

`geode-shell/src/shell/objectdialog.rs`, split pure core / gpui shell
exactly as `picker.rs` and `keybindings_view.rs` are.

```rust
pub enum Stage {
    Browse,
    Edit { object: String },
}

pub struct ObjectDialogState {
    pub domain: Domain,
    pub stage: Stage,
    /// Index into the *filtered* list for the current stage — the
    /// palette's convention, shared by every list surface here.
    pub selected: usize,
    /// Mirrored from `ShellView::dialog_input`, like every other dialog.
    pub query: String,
    /// The object being edited, unsaved. `None` in `Browse`.
    pub draft: Option<Draft>,
}

pub struct Draft {
    pub name: String,
    pub fields: Vec<Field>,
    pub is_new: bool,
}
```

Browse rows are **derived fresh from `Config` on every render and every
keystroke** (`Domain::objects`), never cached — the no-caching contract
`settings_view::derive_rows` and `keybindings_view::derive_rows` already
hold, and the reason those dialogs cannot show a stale value. Only
`draft` is stored, because only it has no source of truth to derive
from.

### 3.1 Field kinds

The closed vocabulary an object is built from:

```rust
pub enum FieldKind {
    Text(String),
    Number { value: i64, min: i64, max: i64 },
    Bool(bool),
    Choice { options: Vec<String>, selected: usize },
    MultiChoice { options: Vec<String>, ticked: BTreeSet<String> },
    OrderedList { items: Vec<ListItem> },
}

pub struct ListItem {
    pub name: String,
    pub included: bool,
    pub width: Option<u32>,
}
```

`ListItem` is deliberately a fixed, bounded shape rather than general
nesting: a view's columns and a grouping's dimensions are the only
ordered lists in the config model, and both fit it. Generalising to
arbitrary sub-fields would buy nothing today and would make the edit
stage recursive.

`OrderedList` is the only kind with a single-domain risk, so Views is
built first (§14): if the vocabulary is wrong, it is wrong before three
more adapters depend on it.

### 3.2 Keys

These dialogs are **modal surfaces** under
`docs/superpowers/specs/2026-09-08-geode-dialog-interaction-model-design.md`,
which governs the vocabulary and must land first. They open in normal
mode; `/` enters filter mode; letters are verbs.

An earlier draft of this section made every verb a row, because with an
always-focused `Input` the key space was exhausted and there was nowhere
else to put them. Two prototypes changed that: the row form makes the
list **reflow while you edit** (a `Save changes` row appearing under the
cursor the moment a draft goes dirty), and dropping the focused input
frees every letter. Verbs are now keys *and* buttons. (The reflow
argument is now moot from the other side too: there is no save row,
because there is no save — see the amendment below.)

| Stage | Keys |
|---|---|
| Browse | `/` filter · `j`/`k` move · `enter` open · `n` new · `escape` close |
| Edit | `/` filter · `j`/`k` move · `space` include · `shift+j`/`shift+k` reorder · `i` edit text · `d` delete · `r` revert · `escape` back |

**There is no `s`.** An earlier build of this section had one, and
staged every field edit behind it; §3.2's amendment below records why
that was replaced by instant application, and §7.1 records how.

Every verb that acts on the **object** is also a button in a bar pinned
below the row list, labelled with its own key: `d`, `r`. That is what
makes a letter verb discoverable and mouse-reachable — a key alone has
no clickable target, and every other verb in Geode's dialogs has one.
`Revert to desk` appears only when the object is overridden. The bar
holds only the destructive and structural verbs, because they are the
only ones left that do not simply happen when you press them.

As built, the verbs that act on a **row** — `space` (include) and
`shift+j`/`shift+k` (reorder) — have no button. An earlier draft of this
paragraph claimed every verb in the table above has one; it does not.
Nothing is unreachable by mouse as a result: the bar is about actions,
and PHILOSOPHY's requirement runs the other way (every action must be
keyboard-reachable). A row verb's natural mouse gesture is the row
itself — a click on the tick, a drag on the handle — not a bar button
that would have to ask which row it meant, so the honest statement is
the one above rather than three more buttons.

`Copy to user layer` is a **confirm**, not a verb and no longer a
label. It arms when the edit just made would fork the object — a
`Doc`-destined field (§4.1) changed on an object whose winning layer is
builtin or desk — and it is the one edit in these dialogs that asks
before acting. Everything else applies on the keystroke.

It asks because a fork *freezes*: a user-layer copy of a desk view stops
receiving the column the desk adds next week, and that cost lands weeks
after the keystroke that caused it. Nothing else on this surface has
that shape — order, inclusion and width fork nothing (§4.1) — so nothing
else asks.

Declining puts the field back where it was. That is not politeness: with
every other edit applying instantly, a declined fork would otherwise be
the one value on screen that is neither applied nor persisted, which is
exactly the state this design exists to make unreachable.

Earlier drafts of this paragraph made `Copy to user layer` a *label* on
`s`, replacing `Save changes` when the save would fork. Both halves are
gone with `s` itself; what survives is the warning, which was the part
that mattered.

The bar sits outside the scrolling list, so **the row list never changes
length as you edit**.

**Amendment: field edits apply instantly. There is no staging, no save
row and no `s`.**

This section originally said the opposite — edits stage into the draft
and write only on `s`, so that a write per keystroke could not fire the
watcher mid-edit and reload a half-finished object. That reasoning
identified a real hazard and picked the wrong cure. The hazard came from
*applying through the disk*: the app wrote a file, the 500 ms watcher
noticed it, and the loader read every layer back to rebuild a config the
app could have built from documents it already held. Staging did not fix
that round trip; it just made the user press another key first, and then
still wait half a second to see whether anything had happened.

The cure is to apply in memory (§7.1). A field edit now:

1. changes the draft — still the edit buffer, still where the cursor and
   the row structure live, and **what the edit stage paints**, so the
   dialog shows the change on the keystroke;
2. folds the changed object into one pending batch;
3. and when the 250 ms window closes, that batch is written into the
   in-memory user-layer `LayerDoc`, re-merged through the loader's own
   `Config::from_docs`, handed to the same applier the watcher uses, and
   written to the file — all together.

**"Instant" is the dialog, not every downstream consumer.** Applying the
merged config per keystroke would emit `ShellEvent::ConfigReloaded` per
keystroke, and the app bridge turns that into fresh `ViewSpec`s — so a
held key would make every blotter tile requery at the OS key-repeat rate,
against a §7.1 budget of 50 ms at 1M rows. The dialog's own response is
free; the world catching up is not, so the world catches up on the same
timer the file does. The blotter updating a beat after the dialog is
correct behaviour, not a compromise.

Nothing a trader can see waits on a file. The half-finished-object
hazard is gone rather than deferred: the object never travels through
disk to reach the screen, and the watcher's reload of our own write is a
no-op (§7.1).

The draft's **baseline** moves in step 2 — as the keystroke is
accounted for, not as the flush lands — so "dirty" is a state that
exists only within the keystroke that changed a field. That is why the bar has no save row
and why the row list no longer reflows as you edit.

`escape` follows the interaction model's ladder (§5 there): filter mode
→ normal mode keeping the query, → clear the query, → back a stage, →
close. Leaving `Edit` no longer confirms: there is nothing unsaved to
discard, and asking anyway would teach a trader that their changes might
not have landed.

### 3.3 Editing a field

`space` is the universal "change this value" key in normal mode:

- `Bool` — toggles.
- `Choice` — steps forward, `shift+space` back, wrapping — the same
  stepping `settings_view::step` performs, on a key that is free here.
- `Number` — steps by the field's increment, clamped to `min`/`max`.
- `MultiChoice` — ticks the highlighted option; `ctrl+a` ticks all
  shown and `ctrl+x` clears, exactly as `picker`'s values stage does.
- `OrderedList` — `space` toggles `included`, and `shift+j`/`shift+k`
  move the item. An earlier draft needed a pick-up sub-mode here
  (`enter` to grab, arrows to move, `enter` to drop) purely because no
  key was free; the interaction model deletes it. Width is a `Number`
  sub-row shown under an included item.
- `Text` — `i` edits the value in place, `escape` leaves the field. An
  earlier draft retargeted the always-focused filter input at the field,
  which was this spec's least certain interaction and its open question
  2; normal mode removes the need for it.

## 4. The adapter seam

An **enum, not a trait object**: the set is closed at five, so matching
stays exhaustive, the pure core stays `dyn`-free, and every adapter is
unit-testable as a plain function.

```rust
pub enum Domain {
    Views,
    Sources,
    Scopes,
    Groupings,
    /// Read-only (§9): `datasets` and `dimensions`.
    Schema,
}

impl Domain {
    fn doc(self) -> &'static str;
    fn objects(self, config: &Config) -> Vec<ObjectRow>;
    fn fields(self, config: &Config, object: Option<&str>) -> Vec<Field>;
    /// `false` for `Schema`: the scaffold then shows no `+ New` and no
    /// action block, and a field edit cannot apply at all rather than
    /// being merely refused.
    ///
    /// NOT BUILT as of Part 2a — every domain so far is writable, so
    /// nothing has needed it. It arrives with the schema inspector
    /// (§9) in Part 2b, which is its only consumer.
    fn writable(self) -> bool;
    fn to_table(self, draft: &Draft) -> toml_edit::Table;
    fn validate(self, draft: &Draft, config: &Config) -> Vec<Diagnostic>;
}

pub struct ObjectRow {
    pub name: String,
    pub summary: String,
    pub layer: Layer,
    pub overridden: bool,
    pub drifted: bool,
}
```

One module per domain under `shell/objectdialog/`, holding those five
functions and nothing else. The scaffold matches on `Domain` exactly
once per function.

### 4.1 Destinations

Every field carries where it is written, which is how the Views
presentation split becomes mechanical rather than a special case inside
one adapter:

```rust
pub struct Field {
    /// The TOML key path within the object: `columns.3.width`.
    pub key: String,
    pub label: String,
    pub kind: FieldKind,
    pub dest: Destination,
}

pub enum Destination {
    /// The domain's own doc, user layer.
    Doc,
    /// `view_presentation.toml`, user layer.
    Presentation,
}
```

On save the scaffold groups a draft's fields by `dest` and makes one
`config_write::edit` call per group. Nothing in the scaffold besides
this enum knows `view_presentation.toml` exists.

This is what keeps the commonest edit cheap. Dragging a column width
writes only `view_presentation.toml`, which is merged *over* the view,
so the view itself is not overridden and the desk's later changes to it
still reach the user. Only a definitional change (dataset, column set)
forks the object.

## 5. Layer, override, revert and drift

### 5.1 Reading the markers

No new machinery. `Config::layered_docs(name)` returns the per-layer
docs in Builtin → Desk → User order, and every doc a dialog edits is
atomic at depth 1 (`config::merge::atomic_depth`: `views`, `layouts`,
`groupings`, `scopes`, `datasets`, `sources`, `dimensions`), so
whole-object replacement by name is the merge rule. For each object:

- `layer` is the last layer whose doc contains that name;
- `overridden` is true when the user layer contains it *and* an earlier
  layer does too.

### 5.2 Drift

Overriding an atomic object freezes your copy: if the desk later adds a
column to a view you overrode, you never see it. Detecting that needs
something to remember what the desk's version was at override time.

`overrides.toml`, user layer, read only by the scaffold:

```toml
config_version = 1

["views.tree"]
shadowed_layer = "desk"
shadowed_text = """
dataset = "risk_snapshot"
columns = ["book", "npv", "delta01"]
"""
```

The **canonical TOML text, not a hash**: `DefaultHasher` is not stable
across Rust versions, a crypto dependency is unjustified for this, one
view's text is a few hundred bytes, and keeping the text makes a real
diff free if it is ever wanted.

A sidecar file rather than a key inside the object, because an atomic
doc's reader treats an unknown key as a diagnostic — the marker must not
become an error in the thing it marks.

`drifted` is true when the shadowed layer's object today differs from
`shadowed_text`. The affordance is the marker plus `Revert to desk`;
diffing is out (§1.4).

### 5.3 Revert

`Revert to desk` deletes the object from the user-layer doc and its
`overrides.toml` entry. For a view it also deletes that view's table
from `view_presentation.toml` — old 4c's `:cols reset`, now reachable
from the dialog.

Every write goes to the user layer. Desk and builtin are never written:
`config_write` refuses them rather than attempting and failing, and a
builtin-layer object's edit stage offers `Copy to user layer` in place
of the field rows' save.

## 6. The write door

`geode-shell/src/config_write.rs`:

```rust
pub fn read(services: &ShellServices, layer: Layer, doc: &str) -> io::Result<String>;
pub fn write(services: &ShellServices, layer: Layer, doc: &str, text: &str) -> io::Result<()>;
pub fn edit(services: &ShellServices, layer: Layer, doc: &str,
            f: impl FnOnce(&mut DocumentMut)) -> io::Result<()>;
```

- `write` is one temp-file-and-rename, replacing the three
  implementations named in §1.2.
- `edit` is the `toml_edit` read-modify-write every keyed persist
  already performs, so comments and unrelated keys survive. A file that
  does not parse is refused untouched, as `fontsize::persist_to_user_config`
  already does.
- Only `Layer::User` is writable. The user directory is
  `ShellView::user_dir`, already `Option<PathBuf>`; `None` means every
  write is skipped, the contract the six existing persists share.
- **All three run off the render thread**, staged on the UI thread and
  spawned on the background executor exactly as `ShellView::persist_theme`
  does. PHILOSOPHY forbids stalling the frame, and a config write is a
  file.

The six existing persist paths are migrated onto this door in the same
task that introduces it, so there is one implementation from the start
rather than a seventh alongside six.

## 7. Applying, validation and the outcome of an edit

### 7.1 Applying is the watcher's *applier*; the source is memory

**Amended.** This section originally said "a dialog writes; it does not
apply", and accepted that an edit would appear up to 500 ms after
saving, to be measured before deciding. The measurement was overtaken by
a plainer objection: if a value is already in memory, writing it to disk
so that a poller can read it back and hand it to the code that would
have accepted it directly is not a design, it is a detour.

What is kept, unchanged and deliberately so, is the **applier**.
`shell::hot_reload::apply_reload` is still the only thing that turns a
`Config` into a running state — keymap, theme, pickables, grouping
slots, saved scopes, the frame's `ConfigReloaded`, the restart banner —
and a dialog edit goes through it exactly as a watcher reload does. What
differs between the two is the `Config`'s **source** (memory, not disk)
and **when** it is applied (on the debounce, not on a poll). Nothing
about the merge or the application differs.

What is new is that the loader's two halves are separable
(`geode-core::config`):

```rust
Config::read_docs(&sources) -> (Vec<LayerDoc>, Vec<Diagnostic>)  // disk
Config::from_docs(docs)     -> Config                             // merge
Config::load(&sources)      -> Config                             // read, then merge
```

`Config::load` keeps its signature and its behaviour, so no existing
caller changes. There are three sources of a `Config` and **one merge**:

| Source | Documents from | Merged by |
|---|---|---|
| startup | disk | `Config::from_docs` |
| the watcher's reload | disk | `Config::from_docs` |
| a dialog edit | memory | `Config::from_docs` |

`Config::from_docs` is the only place merging happens anywhere, and it
must stay that way. A second merger written to "optimise" the in-memory
path — patching the already-merged doc in place rather than re-merging
the layers, say — would be free to disagree with the loader about
override order, atomic depth or provenance, and the disagreement would
surface only as a config that behaves differently depending on whether
it was last touched by a dialog or by a file.

Costs, measured rather than assumed (`docs/perf.md`, "Phase 4c"): a
keystroke's pure core is **37 µs** — it merges nothing and applies
nothing — and the debounced flush pays **70 µs** to merge a builtin layer
including the real keymap, plus **20 µs** to rebuild that keymap inside
`apply_reload`, once per 250 ms.

Five consequences, each decided rather than accepted:

- **Persistence is background and feeds nothing back.** The write still
  goes through `config_write::edit`, because the *file* must keep the
  user's comments and unrelated keys — that is what `toml_edit`'s
  read-modify-write is for. Memory and disk both derive from the same
  rendered table; the write's completion never updates memory.
- **A failed write reverts memory, and says so where it can be seen.**
  It is the one thing that can leave a trader looking at a value that is
  not persisted, so the in-memory change goes back to where the batch
  started, through the same applier. Logging and moving on — what every
  other persist path in this crate does — is right only where memory did
  not already apply the change.

  The report goes to the **status bar** (`config not saved — reverted:
  …`), not only to the dialog's own notice. The pending write outlives
  the dialog on purpose — a trader can close the dialog inside the
  debounce window, which is the commonest way to reach this path — so a
  dialog-only notice would be absent exactly when it is needed. The
  dialog's notice is still set when one is open, and the next successful
  write clears the status segment.
- **Coalescing is a 250 ms debounce on the write *and the fan-out*.**
  An earlier build debounced the file alone and applied the merged config
  per keystroke. That protected the cheap side and left the expensive one
  exposed: every application emits `ConfigReloaded`, the bridge re-derives
  views, and every blotter tile requeries — the §7.1 <50 ms operation, at
  the OS key-repeat rate (~100 ms on macOS) under a held key. The
  fan-out and the write are the same event — "the rest of the world
  catches up" — so they share one timer. 250 ms is over twice the
  key-repeat period and half the watcher's poll.

  Write-on-field-commit was the alternative and was rejected: this stage
  has no commit moment, so "commit" would mean "when the dialog closes",
  and a crash would lose edits the screen had shown as applied for
  minutes. A quarter of a second is the whole exposure — and it is a real
  one: **a quit inside the debounce window loses the pending write.**
  Closing that would mean either writing per keystroke (the thrash this
  exists to prevent) or a shutdown hook this shell does not have.

- **An edit made while a write is in flight is not erased by that
  write's completion.** Each keystroke bumps a sequence and folds its
  change into one pending batch; the flush that wakes holding the current
  sequence owns it. The success arm checks that sequence too — without
  it, an edit that arrives during a write is folded into the batch the
  completing write then clears, so it reaches neither memory nor disk,
  and the watcher (woken by the write that did land) reverts memory to
  the older on-disk state. The change would disappear with nothing on
  screen having said so.

- **A pre-existing broken config file does not disable editing.**
  `reload::decide` rejects any `Config` holding an error diagnostic, so
  carrying the previous config's diagnostics into an edit's config made
  one unparseable `*.toml` turn every edit into a silent in-memory no-op
  — while the write still fired, so memory and disk diverged. Those
  diagnostics describe files that were *skipped* and contributed no
  documents, so they are not diagnostics of the documents an edit
  re-merges, and an edit does not carry them. Last-good still guards what
  it is for: `apply_reload` derives the keymap and mod-alias diagnostics
  from the documents themselves, so an edit that really does produce a
  broken config is still rejected, and the watcher restores the
  `config: N error(s)` status on its next poll.

**The self-write reload is proved inert, not suppressed.** The watcher
will see the file this dialog wrote. `apply_reload` decides what a
reload changes by comparing layered documents against the ones already
live (`docs_equal`); the file is `config_write::edit`'s read-modify-write
of the same object value memory holds, and a user-layer document memory
creates carries the same `config_version` stamp `edit` writes at the top
of a file it creates — so every `changed(..)` predicate answers false,
nothing is emitted, requeried or closed, and the reload assigns an
identical `Config`. Suppression was the alternative: it needs a
self-write ledger that must be right about every path a write can take,
including the ones that fail after the entry is made, and a ledger that
is wrong in the other direction silently swallows somebody's real
external edit. The proof costs nothing and cannot go stale; it is
pinned by a test that compares the layered documents across a
self-triggered reload.

A write the *merge* rejects is still on disk and not live, which is what
last-good means. `ShellEvent::ReloadRejected(Vec<Diagnostic>)` (§2.3)
remains the route for reporting that.

### 7.2 Validation before the write

`Domain::validate` runs on every field change, synchronously on the UI
thread, with no debounce:

1. `Domain::to_table` renders the draft to a `toml_edit::Table`.
2. That table alone is wrapped in a `MergedDoc` and passed to the doc's
   own reader — the same `from_doc` the loader uses (`ViewSpec::from_doc`,
   `SchemaSpec::from_doc`, `DerivedDimensions::from_doc`,
   `GroupingSlots::from_doc`, `saved_scopes_from_doc`,
   `SourceSpec::from_doc` after §2.2).
3. Each returned `Diagnostic` attaches to the field whose `key` matches
   its `path` (§8.5), or to the object header when it has none.

Validating the draft alone rather than the merged result is the honest
thing: it is the object being edited. Anything the merge turns up is
reported by §7.1's reload.

Because validation is by construction — a `Choice` offers only datasets
that exist, an `OrderedList` only columns the dataset has — most old
diagnostics become unreachable. What remains is the genuinely
cross-cutting: a source path that does not exist, a duplicate name, a
grouping naming a dimension no grain carries.

## 8. The four domains

### 8.1 Views

- `dataset` — `Choice` over `SchemaSpec`'s datasets. `Doc`.
- `columns` — `OrderedList` over the chosen dataset's columns, plus
  derived dimensions. Item order, `included` and `width` are all
  `Presentation`; adding a column the view did not have is `Doc`,
  because it changes the view's definition rather than its presentation.
- Action rows: `Delete this view`, `Revert to desk` when overridden.

The `Doc`/`Presentation` split on the same `OrderedList` is the subtlest
thing in this design and the plan gives it its own task and its own
harness entries.

### 8.2 Groupings

- `slot` — **display-only `Text`, not the `Number` this sketch says**
  (Part 2a ruling). The slot number is the object's own *identity* — the
  top-level key in `groupings.toml` (`3 = ["book", "lhu"]`) — not a field
  inside it, so editing it is a rename, and renames are unbuilt (below).
- ~~`name` — `Text`.~~ **Not built.** Groupings have no name beyond their
  slot; see the rename ruling below.
- `dimensions` — `OrderedList` over `pickable_columns(config)`, no
  per-item `width`. All `Doc`.

**Renaming an object is unbuilt everywhere, on a Part 2a ruling**, which
is why neither this section's `name` nor §8.4's survives. Under
instant-apply a per-keystroke rename writes a table per prefix while the
trader types through it, orphaning every intermediate key — so a rename
needs a committed-edit vocabulary (`Text` that applies on `enter`, Part
2b) before any domain can offer one. Half-building it on these two
domains alone was rejected.

### 8.3 Sources

- `dataset` — `Choice`. `paths` — `Text`. `readiness`, `priority` —
  `Choice`. `poll_interval`, `pending_timeout` — `Text`, duration-parsed
  by the reader. `batch_pattern` — `Text`. All `Doc`.
- A write here opens the reload prompt of old 4c §5.7: "Sources or
  datasets changed. Reload data now?", backed by the swappable
  `DataHandle` and a `DataService` restart on the background executor.
  **Not "unchanged" — a Part 2b constraint this sketch predates.** That
  prompt was designed to hang on an explicit save keystroke, and there
  is none any more (§16: edits are instant, `s` is deleted). Triggered
  from the debounced flush instead, it would fire mid-typing — once per
  250 ms pause while a trader edits a glob. So **Sources cannot ship
  until `Text` commits on `enter`** (the committed-edit vocabulary §8.2's
  rename ruling also waits on); the prompt then hangs on that commit.
  Sources is the one domain of the five where this is load-bearing
  rather than cosmetic, because its writes restart a service.

### 8.4 Scopes

Deliberately the thinnest, and the one narrowing of "full CRUD":

- **Two read-only `Text` fields, not one** (as built): `Selects`, in the
  scope bar's own `column ∈ values` spelling, and `Text filter`. The
  single `name` field this sketch asked for is not built — see §8.2's
  rename ruling — and a scope carries no as-of to show beside them:
  `Frame::save_scope` saves the `Scope` alone, and `scope_to_table`'s
  three keys (`dimensions`, `text`, `expression`) have no fourth. As-of
  is frame state, not scope state.
- Action rows: `o` (`Replace with the current scope`), `d`
  (`Delete`), and `r` (`Revert to desk`) when overridden.
  **`Load into frame` is not built**, on a Part 2a ruling: it would
  duplicate `:scope load <name>` and the palette's own `scope::<name>`
  action, and it would be the only row in any of these dialogs that
  mutates frame state rather than config — with nothing to confirm,
  since it destroys nothing.

`o` confirms through `Confirm::Overwrite { forks: bool }`, whose payload
`render::arm_overwrite` decides from the object's winning layer so the
prompt can disclose a fork *before* it happens — `o` reaches
`commit_edit` directly and so never passes `commit_or_confirm`'s own
`would_fork` gate (that gate reads the draft's pending writes, which are
empty until the overwrite has been applied). One confirm, not two: the
fork is disclosed in the prompt rather than asked as a second question,
keeping §16's single fixed-length `Confirm` row.

A scope's *values* are not edited here. The dimension picker and
`:scope save` already author them, and a `MultiChoice` editor per
dimension would duplicate that surface and drift from it. Create is
"scope the frame, then save it under a name", which is the workflow that
already exists; the dialog adds the management half that did not.

### 8.5 `Diagnostic.path`

```rust
pub struct Diagnostic {
    pub severity: Severity,
    pub layer: Option<Layer>,
    pub file: Option<PathBuf>,
    /// The TOML key path of the offending value, dotted, when the
    /// reader knows it: `views.tree.columns.3.name`.
    pub path: Option<String>,
    pub message: String,
}
```

Readers fill `path` where they already know the key. Old 4c resolved it
to a text span; here it selects a **field row**, which is a better fit —
a field always exists for a key the reader named, whereas a span
required the offending text to still be on screen. `Display` appends
` (at path)` when set, so the diagnostics module and stderr gain it too.

## 9. The schema inspector

`config::schema` opens the same scaffold as `Domain::Schema`, whose
`writable()` is `false` (§4 — neither `Domain::Schema` nor `writable()`
exists yet; both are Part 2b), over `datasets` and `dimensions`: browse
the datasets, open one to see its
columns with type, role, grain, and the `categorical`/`textual` flags,
each row showing the layer it came from (`Config::explain`).

Read-only because a schema is the desk's contract with the data, a
`datasets` change is restart-required (Phase 3 §4.5), and the edit and
its effect would be far apart. The other three dialogs read it to build
their `Choice` and `OrderedList` options, so it is the inspector for a
model they already depend on rather than a fifth editor.

## 10. Actions, keys and session

Five palette actions, no default key bindings:

| Action | Title | Category |
|---|---|---|
| `config::views` | Views and columns | Config |
| `config::sources` | Sources | Config |
| `config::scopes` | Saved scopes | Config |
| `config::groupings` | Grouping slots | Config |
| `config::schema` | Schema (read-only) | Config |

No defaults, because `keybindings::open` has none either — it is
palette-only, and the user binds it if they want it (this repo's own
`keymap.toml` binds `ctrl+.`). Adding five default chords to an already
crowded map would be a worse default than discoverability through
`ctrl+k`.

Every one opens through `dialog::open_shell_dialog_with_key`, the
mandatory door (CLAUDE.md), which cancels pending keymap sequences and
closes the palette.

**Nothing persists to the session.** A modal does not survive a restart,
which is true of every other modal in this shell.

## 11. Error handling

- **A write that fails** (permissions, a full disk) **reverts** — as
  built, not "leaves the draft intact so it can be retried", which
  described the deleted staged-save design. `apply::revert_failed_write`
  restores the pre-edit documents through `apply_reload`, rebuilds the
  draft from the config that just went back (so no row keeps painting a
  value the file refused), and reports on **`ShellView`'s status bar**,
  not only the dialog's notice: `pending_config_write` outlives the
  dialog that started it on purpose, so the commonest way to reach this
  path has no dialog left on screen. It is never silent, unlike the six
  existing persists, which
  warn to stderr — those keep that behaviour when called from their own
  paths and gain the dialog's reporting when called from one.
- **A file that does not parse** is refused untouched by
  `config_write::edit`, and the dialog says so rather than overwriting a
  file the user hand-edited into a broken state.
- **A rejected reload** keeps last-good and reports through
  `ReloadRejected` (§7.1).
- **A missing `user_dir`** disables saving entirely; the dialogs open
  read-only and say why.

## 12. Performance

- Validation is synchronous on the UI thread with no debounce, inside
  the §7.1 <8 ms pure-UI budget. A `from_doc` over one object is
  microseconds; the plan measures the largest shipped doc to confirm it,
  the same gate old 4c §5.2 set for its own per-keystroke parse.
- Browse rows derive fresh on every keystroke. That is a
  `layered_docs` walk plus a `listfilter::rank`, the same shape
  `keybindings_view` already does over a much longer list.
- Every write is off the render thread (§6).
- `overrides.toml` is read once per reload with the rest of the config,
  not per keystroke.

## 13. Tests and the harness

Test weight follows spec §10.3 — the pure cores carry it.

**Pure, no window:** every `Domain` function; the stage machine and its
key vocabulary; `FieldKind` editing (step, tick, reorder, clamp); the
`Doc`/`Presentation` grouping on save; override and drift derivation
from a fixture `Config`; `config_write::edit`'s comment and
unrelated-key preservation, and its refusal of an unparseable file.

**`TestAppContext`, real key dispatch:** opening each dialog through its
action; the `+ New` and action rows committing on `enter`; the `Text`
field's input retargeting and its restoration of the filter; a save
reaching `config_write` (through a temp `user_dir`) and the resulting
file's contents; `ReloadRejected` surfacing on the header.

**Mutation entries** for every behaviour above, per CLAUDE.md, with
particular attention to the ones a green suite would not see:

- a `Presentation` field written to the `Doc` destination (which would
  fork a desk view on a column drag — the failure §4.1 exists to
  prevent);
- `overridden` computed from the winning layer alone, ignoring whether
  an earlier layer defines the object;
- `drifted` compared against the user's own text rather than the
  shadowed layer's;
- revert deleting the doc entry but not the `overrides.toml` entry;
- validation run against the merged doc rather than the draft alone;
- ~~a field edit writing immediately instead of staging into the
  draft~~ — **void**: staging and the `s` key are deleted (§16), so this
  names machinery that no longer exists. Its replacement, built in Part
  2a, is that an **error-severity diagnostic blocks the batch**, checked
  inside `apply::commit_edit` itself rather than only by its caller, so
  no call site can route around it;
- `escape` on a dirty draft discarding it without the confirm row;
- a held `OrderedList` item moving the selection rather than the item;
- `config_write` writing in place rather than temp-and-rename.

## 14. Sequencing

One branch per task group, reviewed and merged on its own, following the
working rhythm: worktree, review per task, all five CI checks green on
macOS and Windows, `--changed` mutation after every task, the full
harness at branch end.

0. **The interaction model lands first**, on its own branch — the mode
   machinery and the keybinding dialog's migration, per that spec's §13.
   Nothing below starts until it merges, because every task after this
   one assumes its vocabulary.
1. **The write door.** `config_write`, and the six existing persist
   paths migrated onto it. No new UI. This lands first among 4c's own
   tasks because everything writes through it and because it is the one
   task that can regress existing behaviour.
2. **`SourceSpec::from_doc` moves to `geode-core`** (§2.2), with
   `geode-data` re-exporting. Its own task because it touches the crate
   graph.
3. **The scaffold plus the Views adapter.** Views leads (§3.1): its
   `OrderedList` with the `Doc`/`Presentation` split is the hardest
   shape, and it either proves the field vocabulary or forces it to grow
   before anything depends on it. `ViewPresentationSpec` and the
   loader merge land here.
4. **Groupings and Scopes.** The two thin adapters, on a vocabulary the
   previous task settled.
5. **Sources**, with the reload prompt and swappable `DataHandle` of old
   4c §5.7.
6. **The schema inspector**, read-only mode over the same scaffold.
7. **Docs and harness.** `CLAUDE.md`, this spec reconciled with what was
   built, harness entries reviewed as a set.

## 15. Open questions

None block the plans. Recorded so they are not rediscovered.

1. **The 500 ms apply latency** (§7.1). Measured on a display before
   deciding whether a post-write watcher nudge is worth its complexity.
2. **`space` as the toggle key** (§3.3). Free in normal mode and
   universal for checkbox lists, but also the key most likely to be
   pressed by someone who thinks they are still typing. The mode
   indicator is the defence; `x` is the fallback. Carried from the
   interaction model's own risk 3, and settled there, not here.
3. **Whether `layouts` deserves a sixth dialog.** It is an atomic doc
   like the rest, but nothing authors layouts by hand today — the
   session does it. Left out until there is a story for it.
4. **Drift on a `Presentation` file.** `view_presentation.toml` is
   merged over the view rather than replacing it, so it does not fork
   and cannot drift in the §5.2 sense. If a desk ever ships its own
   presentation layer, this needs revisiting.

## 16. As built — Part 1

Tasks 1–5 built the write door, the `SourceSpec::from_doc` move,
`ViewPresentationSpec`, and the scaffold's browse and edit stages over
`Domain::Views`. A later wave, on a user ruling, replaced the staged
save with instant in-memory application — §3.2 and §7.1 carry that
amendment in full, and the first bullet below records what shipped.
Where the sketch above and the shipped code disagree, this section is
the correction.

- **`hidden` lives on `ColumnPresentation`, not on `ViewColumn`.** §3's
  `ListItem` sketch implied a field shared with the view's own column
  type; `ViewColumn` (`geode-core::view`) is an enum
  (`Dimension`/`Measure`/`Derived`) with no struct fields to add one to.
  `hidden: Option<bool>` sits on `ColumnPresentation` instead, beside the
  `width` it already carried, and both merge into `ViewSpec.presentation`
  the same way `format`/`label` already did. A hidden column deliberately
  stays in `ViewSpec.columns` — the compiler still selects it — so
  unhiding is free and no query changes shape when a column is hidden.
  It is dropped in exactly one place instead: `ColumnPlan::build`
  (`geode-blotter/src/core/plan.rs`), which is what the blotter paints.
  That one `continue` is the whole of §1.3's "hiding one … and the
  blotter reflects it without a restart" — without it every other part
  of the chain (the dialog, the file, the reader, the merge) works and
  the column is still on screen.
- **`width` is `f32`**, matching `ColumnPresentation::width`
  (`view.rs:148`), not §3.1's `Option<u32>` sketch. `ListItem.width`
  follows suit.
- **A presentation save writes only what the trader changed.** §4.1
  splits by *field*; that is not fine enough for `columns`, whose
  `order` and `width` both arrive from the **effective** view — the one
  `views.toml`'s own widths and column order are already merged into. A
  save that wrote every column's position and every declared width back
  would pin the desk's layout for that trader against the desk's later
  changes: the same freeze `Destination` exists to prevent, one
  field-granularity down, fired by the commonest edit there is.
  `views::presentation_table` therefore compares both against
  `doc_baseline` — the order and widths this same save leaves in
  `views.toml` — and omits what still matches. `hidden` needs no such
  comparison: nothing but `view_presentation.toml` can set it.
- **`overridden` counts a user-layer `view_presentation.toml` entry**,
  not just a user-layer entry in the domain's own doc. §5.3 assumed
  presentation always accompanies a doc override; §4.1's split
  guarantees the opposite — hiding a column writes presentation and
  forks nothing — so markers read off the `views` doc alone answered `r`
  with "*tree* has no user override to revert" while the file `r` would
  have removed sat on disk. `derive_rows` takes the presentation doc
  too; the "some earlier layer defines it" half is still read off the
  domain's own doc, because that is the half guaranteeing a revert
  leaves an object behind. `d` is unchanged and still gated on the
  winning layer — presentation forks nothing, so there is no *view* of
  the trader's to delete — but its refusal now names `r` rather than
  claiming they have nothing.
- **`drifted` is present on every `ObjectRow` but is always `false`.**
  §5.2's `overrides.toml` — the sidecar the scaffold would read to
  compare an override against what it shadowed — is not built in Part 1,
  so there is no honest way to compute drift yet; a stand-in derived from
  the live config would mark every deliberate customisation as drifted.
  It is a Part 2 item, below.
- **Config edits are instant; the save key is gone.** The largest
  correction in this section, and the one §3.2 and §7.1 are amended for
  rather than annotated. What shipped:

  - `Config::load` split into `read_docs` (disk) and `from_docs`
    (merge), with `load` unchanged as read-then-merge, so no existing
    caller or test moved. `Config::all_docs` hands the layered documents
    back for re-merging. `load_layer` was left under its own name: it
    already **was** the disk half, so the split only had to lift the
    merge out beside it.
  - `shell::objectdialog::apply` writes the edited object into the
    in-memory user-layer `LayerDoc`, re-merges through `from_docs`, and
    calls `hot_reload::apply_reload`. No disk read on that path — pinned
    by a test that plants a decoy `views.toml` the running config has
    never loaded and asserts the edit does not pick it up.
  - **The merge, the application and the write all ride one 250 ms
    debounce; only the draft moves on the keystroke.** A first build
    applied per keystroke and debounced the file alone, which protected
    the cheap side: `apply_reload` emits `ConfigReloaded`, and every
    blotter tile requeries on it. Ruling: *instant* means the dialog
    responds instantly, not that every downstream consumer re-derives per
    keystroke. Measured after the change (`docs/perf.md`): 37 µs per
    keystroke, 70 µs merge + 20 µs `build_keymap` per flush.
  - The file write stays `config_write::edit` on the background
    executor, keyed by `(doc, object)` so a debounce window spanning two
    objects still lands both. Both ends of the flush check the batch's
    sequence: `promote` before doing the work, and `finish_flush` before
    clearing it. **The second check is what stops an edit made while a
    write is in flight from being erased by that write's completion** —
    without it that edit reaches neither memory nor disk, and the
    watcher then reverts memory to the older on-disk state.
  - **An edit carries no diagnostics forward.** `reload::decide` rejects
    any config holding an error diagnostic, so carrying the previous
    config's made a single unparseable `*.toml` turn every edit into a
    silent in-memory no-op while the write still fired. They describe
    files that were skipped and contributed no documents; they are not
    diagnostics of the documents being re-merged.
  - **A failed write reports to the status bar**, not only to the
    dialog's notice: the pending write outlives the dialog on purpose, so
    the commonest way to reach that path has no dialog left on screen.
  - `Confirm::Discard`, `Draft`-staging and the `s` verb are deleted.
    `Confirm::Fork` replaces the `Copy to user layer` label.
  - **An empty rendered table is written as an absence.**
    `views::presentation_table` renders empty whenever the trader's
    presentation matches the view's own doc, which under instant editing
    is one keystroke away — unhide the last hidden column and there is
    nothing of theirs left to record. Written literally that produced a
    bare `[tree]` in `view_presentation.toml`: a table that says nothing,
    which `ViewPresentationSpec::apply` then reports as a stale entry.
    That artefact had been seen in a real user's file and attributed to
    the staged-save path's baseline handling; the machinery blamed for it
    is deleted, and the same rendering now removes the object from the
    document instead, in memory and on disk alike. It is the one place
    where making edits instant made a latent defect routine, and it is
    closed by construction rather than by hoping the case is rare.
- **`d` (delete) and `r` (revert) both confirm**, and now so does a
  fork. §3.2 originally mandated a confirm only for discarding a dirty
  draft. Delete and revert both remove a file's table outright and
  neither is undoable from inside the dialog; a fork is not destructive
  today but freezes the object against the layer that maintains it,
  which is worse for being invisible until weeks later. All three share
  the one `Confirm` row, which replaces the action bar rather than
  joining it, so the row list never changes length. Discard is gone with
  the staging it guarded.
- **Diagnostics attach to the object, not to the field whose `key`
  matches `path`** (§7.2 step 3, §8.5). **`Diagnostic.path` now exists**
  — `path: Option<String>` and `with_path` landed with the Phase 4b
  merge, *after* this section was written, so the sentence that used to
  stand here ("`Diagnostic` has no `path` field yet") is false and is
  corrected rather than kept. The remaining blocker is the other half:
  no *reader* fills it in, so there is nothing for a field row to match
  on. Filling it (`ViewSpec::from_doc` and its siblings) is Part 2b.
  Validation still runs (§7.2 steps 1–2); its diagnostics render against
  the object header meanwhile.
- **The write door takes a `user_dir`, has no `read`, and returns
  `Result<(), String>`.** §6's signature block is a sketch and all three
  of its details are false as built (`geode-shell/src/config_write.rs`):

  ```rust
  pub fn write(user_dir: &Path, layer: Layer, doc: &str, text: &str) -> Result<(), String>;
  pub fn edit(user_dir: &Path, layer: Layer, doc: &str,
              f: impl FnOnce(&mut DocumentMut)) -> Result<(), String>;
  ```

  `read` was cut by ruling: nothing needs it. Every caller either has a
  `Config` already (which holds the parsed doc) or wants `edit`'s
  read-modify-write, and a `read` beside them would be a second way to
  get a doc's text with no reader to keep it honest. `&ShellServices`
  became `&Path` because the path is all the door uses and a write runs
  on the background executor, where a whole services struct cannot
  follow. `io::Result` became `Result<(), String>`: two of the three
  implementations this replaced already returned messages naming the
  path and the failing step, and `edit`'s parse refusal is not an
  `io::Error` at all.
- **There is no `/` in the edit stage**, though §3.2's key table lists
  one. Filtering field rows would need a second cursor space — a
  filtered position beside the draft's own row index — and would make
  `shift+j`/`shift+k` reordering ambiguous, since the visible neighbour
  would not be the underlying one. Entering the stage clears the query
  instead, which is also what makes the escape ladder land on
  `PreviousStage`: with no query to clear, `escape` has nothing to spend
  on `ClearQuery` first. `/` in the edit stage is claimed rather than left
  to fall through: it is bound to `NormalCommand::EnterFilter` and answers
  with a notice explaining that this stage's rows are not filtered, so it
  reads as a deliberate refusal rather than the dialog having stopped
  responding.

### Unmet done-state items, named for Part 2

§1.3 claims three things Part 1 does not deliver. Each is blocked on a
specific prerequisite no task in this plan built:

1. **A field's diagnostic shows on that field's row.** Blocked on
   `Diagnostic.path` (§8.5) — see above. Needs every reader
   (`ViewSpec::from_doc` and its siblings) to fill it in before the
   object dialog can match a diagnostic to the field it names instead of
   showing it against the object header.
2. **Setting a width.** `width` is displayed and preserved across a save
   — read from `ColumnPresentation::width`, written back to
   `view_presentation.toml`, surviving an edit that was about something
   else — but nothing in Geode can *set* it: the blotter reads
   `presentation.width` and nothing writes it, and old 4c's `:cols save`
   command, the only other thing that ever set it, was deleted with that
   design. Needs the `Number` sub-row §3.3 describes under an included
   `OrderedList` item, made writable.
3. **Reverse stepping.** **Closed by Part 2a** — see §17's first bullet;
   the rest of this item is the state it was closed from, kept because the
   reasoning is what decided the shape of the task.
   §3.3 steps both `Choice` and `Number` with `space` forward and
   `shift+space` back; `dialogmode::normal_command` had no `shift+space`
   case at all — its shift branch handled only `j`/`k`/`g` — so *every*
   steppable kind was forward-only: `Choice` wrapped forward with no way
   back, and `Number` stepped forward only, clamping at `max` with no way
   back down.
   That was one missing key in the interaction model, not a per-kind gap,
   so the task was `shift+space` itself, with `Choice` and `Number` as its
   two consumers — implementing it for `Number` alone and leaving
   `Choice` still forward-only would have satisfied the letter of a
   `Number`-only task name while missing half the defect. Whichever
   adapter builds the first real `Number` field would otherwise have
   inherited a field that can be raised and never lowered. Views exercises
   `Choice` but never needs to step it backward, and has no `Number` field
   at all, so nothing in Part 1 forced either half into view.

### Deferred, and not blocked on anything

**`ShellEvent::ReloadRejected(Vec<Diagnostic>)` (§2, amendment 3) is not
built.** §7.1 and §8.5 both rely on it — it is how a dialog says
`saved · rejected: n errors` about a write the *merge* refused, which
last-good semantics leave on disk and not live. No Part 1 task named it,
so it fell through both the plan's task list and its "deliberately out"
list. It is not blocked on a prerequisite: `ShellEvent` already carries
`ConfigReloaded`, and `hot_reload::apply_reload` already has the
diagnostics in hand at the point it decides to keep the last good
config.

Its cost has risen since the sketch. `bridge.rs:303` discards the
presentation diagnostics on the reload path — `let (views, _) =
load_views(config)` — exactly as `data_setup` does at startup, where
they *are* reported. So a `view_presentation.toml` entry naming a view
that no longer exists warns once at startup and is silent through every
reload afterwards, including the reload the Views dialog's own write
triggers. With no `ReloadRejected` and no `Diagnostic.path`, a stale
presentation name is silent in every path after startup: the trader
renames a view, their personal order and hidden columns quietly stop
applying, and nothing anywhere says so. Whichever of the two is built
first should carry the other's fix with it.

## 17. As built — Part 2a

Part 2a added reverse stepping, the error-diagnostic gate, and the two
thin adapters (`Groupings`, `Scopes`). §8.2, §8.4, §11 and §13 above are
corrected in place where they disagree with what shipped; this section
records what has no home in those sections. The three dialogs that
remain — Sources, the schema inspector, per-field diagnostics, width
editing — are Part 2b.

- **Reverse stepping is one key, not a per-kind gap.**
  `NormalCommand::ToggleBack` (`shift+space`) joined `dialogmode.rs`, and
  both `Choice` and `Number` consume it through `Draft::step_selected`.
  §16's "Unmet done-state items" item 3 is closed. Implementing it for
  `Number` alone would have satisfied a `Number`-shaped task name while
  leaving `Choice` wrapping forward with no way back, which is why the
  task was the key rather than either consumer.
- **The error-diagnostic gate lives inside `apply::commit_edit`.**
  `commit_or_confirm` keeps its own earlier `blocking_diagnostic` peek,
  but that is UX only — it avoids asking a trader to confirm a fork and
  then refusing it. The safety property is the check *inside*
  `commit_edit`, which both of its call sites therefore pass through. A
  `Severity::Warning` never blocks; only `Severity::Error` does, which is
  what makes `Domain::Scopes` unblockable in practice —
  `saved_scopes_from_doc` only ever warns.
- **`d`/`r` join the same batch as a field edit**, through
  `apply::commit_removal`, and are as instant as everything else. Two
  deliberate asymmetries: a removal skips `blocking_diagnostic` entirely
  (the diagnostic it would be gated on is usually the very thing the
  removal exists to clear), and it flushes with `Duration::ZERO` rather
  than `WRITE_DEBOUNCE`, since a confirmed removal is one already-decided
  act with nothing to coalesce. `commit_removal` takes bare `(doc,
  object)` keys rather than the `ObjectEdit`-valued map `commit_edit`
  fills — a structural choice, so no future caller has a parameter in
  which to smuggle a value past the gate it skips.
- **A grouping's object is a bare array, not a table.** `3 = ["book",
  "lhu"]` is the first adapter whose rendered object is not a
  `toml_edit::Table`, which is why `Domain::to_table`, `set_object`,
  `object_text` and `apply::object_value`/`run_writes` all carry a
  `toml_edit::Item`. The §4 signature block still shows
  `-> toml_edit::Table`; `Item` is what shipped.
- **`Draft` carries `baseline_source` beside `baseline`.** Both Scopes
  fields are painted summaries, so comparing `fields` against `baseline`
  alone could call a real change clean: two different scopes can render
  the same `column ∈ values` line. `is_dirty` is
  `fields != baseline || source != baseline_source`, and
  `writes_by_destination` consults `source` the same way. This was a
  review finding rather than a design decision — the collision is
  unreachable today for values without `", "` in them, but any elision
  added to the summary would have made `o` silently stop writing whole
  classes of scopes, with no test able to see it.
- **An empty rendering means opposite things at the two destinations,
  and the type says so** (whole-branch review, MAJ-1).
  `apply::object_value` takes the `Destination` and returns
  `ObjectWrite::{Set, Remove, Nothing}` rather than an
  `Option<toml::Value>` whose `None` the batch spells "remove this
  object's key". Removing a user-layer key in a domain's *own* doc means
  **inherit the layer beneath**, so collapsing an emptied object to an
  absence there restored the desk's copy of it — a trader who unticked a
  slot's last dimension got the desk's chain back while the edit stage
  kept painting an empty one and `ctrl+3` kept regrouping by it.
  `Destination::Presentation` keeps the collapse, because
  `view_presentation.toml` is an overlay and absence IS the state. The
  other half of the rule is that the unrepresentable state is not offered
  at all: `Draft::step_selected` **refuses** the untick that would empty a
  `Destination::Doc` list (`GroupingSlots::set` refuses an empty chain;
  `from_doc` warns "slot N is empty; ignored"), returning
  `Step::Refused(reason)` which `render::refuse_step` shows with the verb
  that does what the trader meant — `r` restores the desk's copy, `d`
  deletes their own. `Step` replaced `bool` for exactly that reason: an
  inert row and a refused step are different things to say.
- **Two smaller review fixes in the same pass.** `commit_edit` resolves
  `ShellView::user_dir` *before* `Draft::mark_saved`, so a shell with
  nowhere to write leaves the draft dirty rather than making an unqueued
  value the baseline (`queue_batch` now takes the directory as a
  parameter, so no caller can reach the queue without having answered
  that question). And `o` over a scope that already equals the frame's —
  the ordinary state after `:scope load` — says "already matches the
  frame's scope" instead of answering a confirmed verb with silence;
  `commit_edit` returns `None` both for "queued" and for "nothing
  changed", so the no-op is identified at the call site.
- **Still open, and named so it is not rediscovered:** `drifted` is on
  every `ObjectRow` and still always `false` (§5.2's `overrides.toml` is
  unbuilt), and `ShellEvent::ReloadRejected` is still not built — §16's
  "Deferred, and not blocked on anything" note stands unchanged, as does
  its observation that `bridge.rs:303` discards the presentation
  diagnostics on the reload path.

## 18. Part 2 refinement — design, ahead of Part 2b

Approved 2026-09-10, before Part 2b starts. Four things a user found
using Part 2a on a display, each a ruling here rather than a task
detail: what shipped reads like a terminal UI where the mocks did not;
nothing can *create* an object; the edit stage cannot be filtered; and
the Groupings dialog lists only the slots that happen to be configured.
The mock these rulings are measured against is the "Geode Config
Dialogs" artifact of 2026-09-09; its chrome is the target, its
invented palette is not — every colour stays a `cx.theme()` token.

Everything below is scoped to the object dialog and the chrome it
shares. The command palette, the dimension picker, the as-of selector
and the settings dialog are untouched except where a *shared* element
changes underneath them (§18.1). Renaming an existing object stays
unbuilt everywhere (§8.2's ruling), and the `Text` editing vocabulary
stays in Part 2b.

### 18.1 Visual refresh

The chrome changes are made once, in `shell::dialog`, so the
keybindings and settings dialogs inherit them rather than developing a
second look:

- **The title row gains a right-hand slot.** `ShellModal` carries an
  optional `title_extra` builder, rendered between the title and the
  close button. The object dialog puts its *crumb* there — a count in
  the browse stage (`2 views`, `9 slots`, `3 saved`), the slot's chord
  in a Groupings edit (`ctrl+3`) — followed by the mode pill, which
  stops being a row of its own above the filter. The pill's home moves
  for every modal surface, the keybinding dialog included.
- **The frozen filter row shows a placeholder** (`press / to filter`,
  muted) while the query is empty, instead of an empty line under a
  search icon.
- **One `badge` helper beside `key_chip`**: a bordered, mono, small
  pill. Every classification marker wears it — the winning layer
  (`builtin`/`desk`/`user`), `overridden`, `drifted`, and a field's
  destination (`doc`/`pres`) on edit rows. Layer and destination
  badges take the muted pair; `overridden` takes `primary` foreground
  on a `primary`-tinted border, because it is the one marker a trader
  acts on (`r`). Nothing here spends `warning` or `danger`: those stay
  for diagnostics and destructive buttons.
- **Ordered-list items** get a grip glyph, then a tick — `✓` in
  `success` for an included item, `·` in the muted foreground for an
  excluded one — in place of `[x]`/`[ ]`. An excluded item's name stays
  muted, so a glance down a 30-row list has two signals, as today.
- **A section header** (uppercase, letter-spaced, small, muted) sits
  above each ordered list's items, carrying that list's own key hint
  (`Columns — space includes · shift+j / shift+k reorders`). Headers
  are not rows: the cursor skips them, and `Draft::rows` does not know
  they exist.
- **Action-bar buttons are outlined**, key chip first, then the label;
  destructive ones keep `danger`. The confirm block keeps its shape.
- **The footer** keeps its two hint lines and its notice, above a top
  rule, with the mock's spacing.

### 18.2 Creating an object: `n`

§3.2 names `n` and §3 sketches `is_new`; Part 1 built neither because
nothing could commit a name under instant-apply. The ruling is that the
name is typed **once, into a name field, and committed on `enter`** —
the one keystroke at which a name exists, so no prefix is ever written
and the rename ruling is intact.

- `n` in the browse stage puts the dialog into a `Naming` sub-state:
  the filter row becomes the name field, labelled by domain
  (`New view · name`, `New scope · name`), the shared `Input` focused
  and empty. `escape` returns to browse with nothing written.
- `enter` validates the name by the same rule `Frame::save_scope`
  applies — non-empty, not `config_version`, no whitespace, `.` or
  `"` — plus one more: a name any layer already holds is refused with
  a notice (`'tree' already exists — open it instead`), because
  creating over a desk object would be a fork the trader did not ask
  for.
- On a valid name the scaffold builds a draft from the adapter's
  *empty-object* fields (`Domain::fields(config, None)`, which every
  adapter already answers), marks it new, writes it through the
  ordinary batch as a `Doc` write to the user layer, and opens the edit
  stage on it. The object is therefore in memory, on disk and in the
  browse list from the same keystroke; nothing on screen is unsaved.
- `Draft::is_new` exists from this point and is what the edit header
  reads to say `new` beside the layer badge until the dialog is left.

Per domain:

- **Views.** The empty object is the schema's first dataset and no
  columns. This exposes a prerequisite §8.1 states and Part 1 did not
  build: the column list holds only the view's *own* columns, so a new
  view has nothing to tick. The list becomes **two sections** —
  `Columns` (the view's members, tick = shown/hidden,
  `Destination::Presentation`) and `Available` (the chosen dataset's
  other columns plus derived dimensions, in schema order). `space` on
  an available column **adds it to the view**: a `Doc` write, going
  through `Confirm::Fork` when the view is not the user's. `x` on a
  member **removes it from the view**, same destination and same
  confirm. Hidden and removed are different states on purpose: hiding
  never forks, and a desk column the trader hid still comes back when
  the desk changes it. Changing the dataset empties `Available` and
  repopulates it; members that the new dataset lacks stay listed, as
  today, so the diagnostic can name them. `x` is a Views verb only:
  it exists where a list's *membership* is a different thing from an
  item's *inclusion*. In Groupings the two coincide (untick is
  removal), so `x` there is inert with a notice naming `space`.
- **Scopes.** `n` saves the frame's **current** scope under the new
  name — exactly what `:scope save <name>` does, through the same
  `scope_table_as_toml` rendering `o` uses, so the two doors cannot
  write different tables. The empty-object fields are not used here:
  an empty scope is a valid but useless object, and the frame's scope
  is what a trader pressing `n` in this dialog has just built.
- **Groupings.** No `n`; §18.4 makes every slot a permanent row.
- **Schema (Part 2b).** `Domain::writable()` finally gains its consumer
  and answers `false`; the browse stage then shows no `n` in its hint
  row and `n` is inert with a notice.

### 18.3 Filtering the edit stage

`/` enters filter mode in the edit stage exactly as in browse; §3.2's
table always said so, and `enter_edit_stage` forcing `Normal` with an
empty query was Part 1 leaving the rung unbuilt, not a ruling. The
query ranks each row's *label* — a field's label, an item's name —
through `listfilter::rank`, and `Draft::selected` indexes the filtered
list, the palette's convention. Every verb acts on the row under the
cursor by identity: `space`, `x`, `shift+j`/`shift+k` resolve the
filtered index back to an `EditRow` before acting. A reorder while the
filter hides neighbours moves the item past the hidden ones — that is
what reordering a filtered list means — and the notice says so
(`moved past 2 hidden`). `escape` walks the full ladder: leave filter
keeping the query, clear the query, back to browse, close.

### 18.4 Groupings: nine fixed rows

The browse stage always lists slots `1`–`9`, in order, whatever
`groupings.toml` holds. An unconfigured slot's row reads `empty` and
wears no layer badge. Opening it shows every pickable dimension
unticked — the empty-object fields the adapter already produces — and
ticking the first one writes the slot to the user layer, with no fork
to confirm since nothing lies beneath. `d` on a configured user slot
returns the row to `empty` rather than removing it; `r` on an
overridden slot restores the desk's chain as today. Slot `0` does not
appear: `ctrl+0` is `frame::slot_clear`, the view's own grouping, and
`GroupingSlots` stays nine wide — a user ruling of 2026-09-10 against
widening it.

### 18.5 Tests and harness

Pure-core tests: the name check and its already-exists refusal; the
two-section column list, its destinations and the fork/no-fork split
between adding and hiding; filtered edit navigation resolving verbs by
identity; the fixed nine-slot roster with an empty slot's row.
Window tests: `n` end to end in Views and Scopes (the object is in the
browse list and on disk after `enter`), and `/` in the edit stage.
Every behaviour above gets a mutation entry naming the test expected to
catch it (`--anchors-only` before merge, as always). Visual changes
are checked on a display against the artifact, not by test.

### 18.6 As built

- **Groupings' nine rows are the roster, not the config.**
  `Domain::roster() -> Option<&'static [&'static str]>` answers
  `Some(["1"..="9"])` for Groupings and `None` for Views/Scopes;
  `derive_rows` seeds a placeholder row (`layer: None, summary:
  "empty"`) for every rostered name *before* the layered-doc walk, so a
  configured slot overwrites its placeholder in place and an
  unconfigured one survives untouched. `ObjectRow.layer` is
  `Option<Layer>` for exactly this reason — a row can now exist with no
  layer at all — and every reader that used to compare it to a bare
  `Layer` was rewritten to match on `Some`/`None`: `d` on an empty slot
  says `"{name} is empty — tick a dimension to fill it"` rather than
  falling into the generic "comes from the no layer" wording; `o`'s
  fork disclosure and `d`'s delete gate both read `Some(Layer::User)`
  instead of `Layer::User`. Ticking the first dimension in an empty
  slot writes it straight to the user layer through the ordinary
  batch, with no fork question, since nothing lies beneath an absent
  layer to fork from.

- **`check_object_name` (`geode-core::config`) is the one name rule.**
  Trimmed, non-empty, not `config_version`, free of whitespace/`.`/`"`;
  it returns the trimmed name so a caller cannot check one spelling and
  write another. `Frame::save_scope` and the object dialog's `n` both
  call it, so a name `:scope save` accepts is a name the dialog accepts
  and vice versa — the two doors were never allowed to drift.

- **Creating an object is `Stage::Naming`, committed on `enter`, one
  keystroke wide.** `n` in browse swaps the filter row for a name field
  (`dialog::name_row`) and clears the shared `Input` explicitly —
  `begin_naming` only clears the mirrored `query`, and the `Input` is a
  second buffer `set_value` cannot self-correct, so a leftover browse
  filter (typed, then `escape`'d without clearing) would otherwise
  survive into the name field verbatim; a fix round closed this after
  it was caught reachable end to end (`/tr`, `escape`, `n` used to open
  the name field already reading `tr`). `enter` validates through
  `check_object_name`, refuses a name anything already holds
  (`Domain::name_taken`, below), and on success builds a
  draft from the adapter's empty-object fields, marks it `is_new`,
  writes it through `apply::commit_create` — **one `Doc` entry, `queue_
  batch` at `Duration::ZERO`, never through `edits_for`** (the map
  `commit_edit`/`commit_removal` build from the whole draft; a create
  has no baseline to diff against, so it builds its own one-entry map
  directly) — and opens the edit stage on it via `enter_edit_with`. The
  header's `new` badge (`objectdialog-new-badge`) keys on `Draft::
  is_new` **alone**, for the stage's entire life, not on `editing_row
  (shell).is_none()`: `editing_row` derives from `services.config`,
  which trails the zero-debounce flush by at least one executor tick,
  so gating the badge on the row's absence would flicker it off the
  instant the row derived while the object is still, correctly, "new"
  for the rest of the session in that dialog. The same lag is why `d`/
  `r` are silently withheld for that first tick (both gate on
  `editing_row`) — documented in `actions()`'s own doc rather than
  fixed, since there is nothing yet for either verb to act on. `n` is
  inert on Groupings (`"the slots are fixed — open one to fill it"`),
  gated on `domain.roster().is_some()` — the same predicate §18.4's
  nine rows are built from, not a separate Groupings check. The create
  gate throughout is `Domain::roster().is_some()` standing in for the
  `Domain::writable()` Part 2b has not built yet; when it lands, the
  `n`-suppression here and the footer hint that omits `n` for a fixed
  roster should both move onto it. While naming, the browse list behind
  the name field keeps ranking by the typed name (`set_query` mirrors
  into `state.query`, not `Draft::query`, because `Stage::Naming` is
  not `Stage::Edit`) — deliberate: a name close to an existing object's
  stays visible as a near-collision warning while it is typed, not just
  refused after the fact on `enter`.

- **"A name any layer already holds" is `Domain::name_taken`, which is
  wider than the browse list.** The refusal used to read the rows
  (`render::derive_rows`), and the rows are `doc`'s layered keys plus
  the roster — so a name held by the user's presentation overlay ALONE
  had no row and was not refused. That is a reachable state, not a
  theoretical one: the desk drops a view the trader had hidden a column
  on, `views.toml` no longer names it at any layer and
  `view_presentation.toml` still does. `n` on that name created a fresh
  user view that immediately inherited the orphaned overlay's
  `hidden`/`order`/`width`, and — being user-only — `r` refused it, so
  no verb in the dialog could clear it again. `name_taken` unions the
  three: the roster, every layer of the domain's own doc, and
  `personalised_names(config, presentation_doc)` — the same one walk
  `derive_rows` uses for its "personalised without overriding" marker,
  factored out so the two answers cannot drift. The notice distinguishes
  the two cases, because the instructions differ: a listed name says
  `'tree' already exists — open it instead`, while an overlay-only name
  says `'gone' has a saved presentation — remove it from
  view_presentation.toml first` — pointing at the browse list would be a
  dead end for a name that is not on it.

- **Views' column list is two `ListItem` blocks, `member` first.**
  `ListItem.member` is definitional (`Destination::Doc`, forks a desk
  view) where `ListItem.included` is presentation (hide/show, never
  forks); Groupings sets `member: true` everywhere, since ticking
  already *is* membership there. `space` on an available row promotes
  it to the end of the member block; `x` demotes a member to the end of
  the available block. `x`'s refusal is decided by the field's own
  `dest`, never by scanning whether every row is currently a member: on
  a `Destination::Doc` list (Groupings) it refuses `"space unticks
  here"`; on an available (non-member) row of a `Destination::
  Presentation` list (Views) it refuses `"not in the view — space adds
  it"`. This is why `x` reads as Views-only in practice without being a
  domain check anywhere — a future `Doc`-backed list with its own
  member/available split would need no new case here, only its `dest`
  set correctly. `ListItem.kind: Option<String>` carries the
  `[[columns]]` `kind` a first-time write of a promoted column needs
  (`"dimension"`/`"measure"`, from `view_column_kind` for an existing
  member or `schema_role_kind` for an available one) — without it, a
  promoted column's kind defaulted to `"measure"` regardless of its
  real role, silently summing a dimension. **Derived dimensions, `Key`
  and `Attribute` columns are not offered in the available block at
  all**: none has an honest name+kind `[[columns]]` spelling the loader
  and compiler resolve (a derived dimension's value comes only from a
  `case` expression over `view.grouping`, never from `view.columns`;
  `Key`/`Attribute` have no `ColumnRole` mapping into `"dimension"`/
  `"measure"`), and writing a dishonest kind is the exact defect class
  this list exists to remove — `schema_role_kind` returns `None` for
  both roles and the dimensions doc is never consulted for the
  available block at all. A demoted column re-enters the available
  block at its *end*, not back at its schema position — `remove_
  selected` pushes it there rather than re-inserting it in place, so
  repeated add/remove cycling does not restore original order.
  Changing the dataset field rebuilds the available block from scratch
  (`views::refresh_available`): members are retained untouched (even
  ones the new dataset lacks, so the diagnostic can still name them),
  the available block is repopulated from the newly chosen dataset, and
  the cursor is re-found by identity (`Draft::selected_row()` before
  the rebuild, `Draft::follow` after) rather than clamped by index —
  the general, identity-based primitive, though at `refresh_available`'s
  one call site (always the `dataset` field, always `rows()`'s first
  element, always first in row-ordered `visible_rows()` once §18.3's
  row-order painting landed) an index clamp and `follow` are provably
  equivalent — the two premises of that proof are now asserted in the
  covering test itself (`rows()[0] == EditRow::Field(0)` and
  `fields[0].key == "dataset"`), so an adapter that grew a field above
  `dataset` would fail there rather than quietly retire the argument —
  which is why **this one call has no mutation entry**: any
  mutation of the `follow` call is unavoidably `SURVIVED` there, and the
  harness's own header calls a `SURVIVED` entry with no discriminating
  test worse than no entry at all. `x` can empty a view's column set
  entirely — unlike `space`'s forward step, which refuses to empty an
  `OrderedList` field, `remove_selected` has no such guard, and a
  column-less view is a valid (if useless) object to the reader — a
  deliberate asymmetry, not an oversight.

- **The edit stage filters through the same `listfilter::rank` browse
  already used**, per §18.3, but with two shapes browse does not need.
  `Draft` carries its own `query: String`; `ObjectDialogState::
  set_query` mirrors the shared `Input` **one-way per stage** — into
  `Draft::query` inside `Stage::Edit`, into `state.query` everywhere
  else — never both, because one `Input` serves two independent filter
  spaces and a two-way mirror let the edit stage's filter leak into the
  browse query underneath it (caught in review: leaving the edit stage
  with a query typed re-entered browse with that same text silently
  applied). The read half of this mirror is now a getter,
  `effective_query`, added by the 2026-09-11 dialog-text-sync amendment
  (interaction-model spec §16.2) for `dialog::sync_dialog_text` to read
  alongside the `Change` subscription — `set_query` is unchanged and
  stays the only writer. Every verb — `space`, `x`, `shift+j`/`shift+k` — resolves
  through `Draft::selected_row()`/`Draft::visible_rows()`, which index
  the *filtered* list, and a reorder that skips hidden neighbours says
  so (`"moved past 2 hidden"`) via `Draft::move_item`'s `Option<usize>`
  skip count. **The edit stage's visible rows are ranked for matching
  but painted in row order, not score order** — `Draft::visible_rows`
  sorts `rank()`'s output by each match's row index before returning it
  — because row order is the only signal separating a list's member
  block from its available block (or a grouping chain's own sequence),
  and fuzzy-score reordering would scramble that signal the moment a
  query narrowed the list. Browse keeps score order, since a browse row
  carries no such structural meaning. This is why the two `visible_
  rows` (the free `render`-adjacent one for browse, `Draft`'s own
  method for the edit stage) are not the same function despite the
  similar name. Making the edit stage reachable in filter mode also
  reached its **mouse** path: `render::on_edit_row_clicked` predates
  §18.3 and focused the shell unconditionally, which after this task
  left the pill reading `filter` and a caret painted over a blurred
  `Input` — one switch, thrown halfway, with every following keystroke
  going nowhere until `escape`. It now reads `state.mode` and focuses
  whichever surface owns that mode, exactly as browse's own
  `on_row_clicked` has since Part 1. **Any new mouse path into either
  stage owes the same read**; focusing the shell is correct only in
  normal mode.

- **Chrome:** `ShellModal.title_extra` (`Option<TitleExtraBuilder>`,
  `dialog::set_title_extra`) puts a builder-supplied element between
  the modal's title and its close button; the object dialog's builder
  paints a crumb (`render::crumb_text` — a count in Browse/Naming,
  `ctrl+{slot}` in a Groupings edit, empty otherwise) followed by the
  mode pill, which no longer paints as its own row above the filter for
  *either* modal dialog — the keybinding dialog's pill moved into the
  title row too, under the same shared slot, and `build_edit` gained a
  pill for the first time (it never painted one before this task; the
  edit stage previously showed no mode indicator at all while browse
  did). `dialog::badge` is the one classification pill — bordered,
  mono, small — every layer/overridden/drifted/destination marker now
  renders through. The frozen filter's placeholder (`"press / to
  filter"`) does not simply key on "frozen and empty": `FrozenFilter
  { query, slash_filters }` adds the second field because a frozen,
  empty query is not always one `/` away from filtering — the
  keybinding dialog freezes its filter while *listening* for a capture,
  where `press_while_listening` swallows `/` as the binding rather than
  opening the filter, and the placeholder used to lie about that
  (painting "press / to filter" underneath a footer simultaneously
  saying "Listening…"). §18.1's "frozen and empty" trigger is corrected
  here to "frozen, empty, **and `/` reachable**"; the keybinding dialog
  passes `slash_filters: state.listening.is_none()`, the object
  dialog's two call sites (no capture state) pass `true` unconditionally,
  and the three live-`Input` filter rows (settings, palette-style
  picker, as-of selector) are untouched (`None`, unaffected either way).
  Section headers are not separate list children: each rides on the
  first *visible* item of its member/available block, folded into that
  row's own element (`v_flex().child(header).child(row)`), so the
  list's child count still equals `visible_rows().len()` at every
  filtered or unfiltered position and `ScrollHandle::scroll_to_item`
  (which indexes children positionally) keeps landing on the row it
  means to. The first attempt at a covering mutation entry for this
  shipped `SURVIVED` and honestly said so — no existing assertion
  distinguishes "header folded into the row" from "header as its own
  child before the row" except which DOM index a scroll target lands
  on — and was replaced before merge with a dedicated cursor-in-view
  test on a >10-row list (`shift+g` to the true last row, asserting its
  bounds fall inside the list's own viewport rather than scrolled past
  either edge), which does catch the mutation. **A maintainer must not
  emit a section header as a sibling `list.child()` call** — it is a
  silent off-by-one against every following `scroll_to_item` index, not
  a visual-only regression.

- **The display check against the 2026-09-09 artifact could not be
  completed in the implementation sandbox.** No window ever painted —
  the sandbox has no window-server interaction available to this
  session (`osascript`/System Events automation both refused) — so
  none of §18.1's visual claims (badge styling, grip/tick glyphs,
  section-header typography, the crumb's exact placement) were checked
  pixel-for-pixel against the mock. Every behavioural claim in this
  section is instead verified against window-test assertions and direct
  code reading. **This is pending on the user's own display** before
  Part 2b starts.

- **Harness:** `zsh scripts/mutation-check.sh --changed=main` on HEAD
  `1c35a74` — 104 entries in files this plan changed, all `caught`, 0
  `SURVIVED`, 411 entries in unchanged files skipped, tree clean
  afterwards. `--anchors-only` reports 515 anchors, 0 stale, 0
  ambiguous. After the 2026-09-11 fix wave: six entries added or
  re-anchored (`a create ignores a name the presentation overlay holds`;
  `an edit-stage click takes the keyboard off the filter`;
  `space`/`shift+space`/`x moves the cursor off screen and leaves it
  there`; `a Key or Attribute column is offered as a dimension`; and
  `n refuses an existing name` re-anchored onto `Domain::name_taken`),
  and `zsh scripts/mutation-check.sh "objectdialog:"` runs all 63 of
  this surface's entries `caught`, 0 `SURVIVED`, in about four minutes.
  `--anchors-only` reports 521 anchors, 0 stale, 0 ambiguous.

- **Deferred minors** (full detail in the plan's ledger,
  `.superpowers/sdd/2026-09-10-phase-4c-part-2-refinement/progress.md`):
  no window test that `d` on a configured user-only slot returns it to
  `empty` (true today by `derive_rows`' seed-before-walk, but untested
  as its own behaviour); `check_object_name`'s mutation coverage is its
  `config_version` clause only; `commit_create`'s unreachable
  `ObjectWrite::Remove` arm (`Destination::Doc` never removes) wants a
  one-line comment; a dataset switch queues a redundant same-bytes
  `view_presentation.toml` write alongside the real `views.toml` fork;
  Scopes' `o` action does not gate on `editing_row` the way `d`/`r` do
  (a freshly created scope's action bar offers "Overwrite from frame"
  before the object is a row at all — harmless, since `arm_overwrite`
  already handles a `None` row, but asymmetric); `Draft::visible_rows`/
  `selected_row`/`follow` allocate a fresh `Vec` per keystroke; and
  `crates/geode-shell/src/shell/tests/objectdialog.rs` has grown past
  3,000 lines, a candidate for a seam split alongside the crate's other
  oversized test files.

- **The final review's fix wave (2026-09-11) closed three of those
  minors and two Importants.** The Importants are the two bullets above
  (`Domain::name_taken`; the edit-stage click's mode read). Of the
  minors: the `Key`/`Attribute` exclusion now has a test and an entry —
  `views::tests::tree_with_two_available_columns` carries an
  `instrument_id` (`Key`) and a `strike` (`Attribute`) for no other
  purpose, since without a column of each role in the fixture the
  exclusion is indistinguishable from an empty match arm. `space`,
  `shift+space` and `x` now scroll the cursor back into view through
  `render::scroll_to_cursor`, which `shift+j`'s arm (the only one that
  ever did) also calls, so a fourth verb that moves a row has one thing
  to call rather than a snippet to copy; the covering tests need a
  fixture of about forty columns, not the section-header test's
  fourteen, because fourteen rows still fit the viewport well enough
  that a promoted row lands back in view by accident. And the two
  `mod.rs` doc comments that justified row-order filtering by "`[ ]`
  rows are indistinguishable" are rewritten onto what §18.1 left true:
  the order is a value `shift+j`/`shift+k` edit, and a section header
  marks where its block BEGINS, so a score sort would put rows under
  the wrong header rather than merely lose a cue.

- **After `space` adds a column, the cursor moves on, not with it (user
  ruling 2026-09-11).** The add branch of `Draft::step_selected` used to
  `follow` the promoted item to its new row at the end of the member
  block, so a trader adding several columns was carried out of the
  available block on every keystroke. It now leaves the cursor at its
  old visible index plus one — the row that was next — clamped to the
  last visible row, so adding the block's last column lands on the row
  that preceded it rather than off the end. The arithmetic is honest
  under a filter because the added item moves *earlier* in row order
  with its label unchanged: the rows ahead of the next visible one are
  the same set, merely reordered. (`x` was ruled on separately later the
  same day — the next bullet — and no longer follows the demoted item;
  this sentence originally said it did.) `shift+space` shares
  the add branch and so behaves the same. Two harness entries guard the
  `+ 1` and the clamp; the scroll-into-view tests for `space` and
  `shift+space` now put the cursor on the viewport's last row and assert
  the *next* row is scrolled in.

- **`x` follows the same ruling (2026-09-11, same day):** the cursor
  stays at its own visible index — the row that was next, since the
  demoted item moved *later* in row order and the rows ahead of the next
  one lost exactly one — rather than following the removed column to
  the end of the available block. Removing the list's last row, where
  the same index would still be on the removed item, steps back one row
  instead (`dd` on a buffer's last line). Because the cursor now holds
  a position that was on screen before the keystroke, the `x` arm's
  `scroll_to_cursor` call (added by the 2026-09-11 fix wave above) is
  gone along with its harness entry — an entry over a call no test can
  see is a lie — and two entries over the rule replace it. `space`'s
  arms keep theirs: an add can land the cursor one row past the
  viewport's bottom.

### 18.7 Groupings: digit jump and the chain field (2026-09-12)

Two additions to the Groupings dialog alone, from a user request the
same day ("we should just be able to hit the number for a slot", and a
faster way than the chooser "to quickly type something like `book / lhu
/ positions`"). Both are built and window-tested; the display check is
pending on a real window, as §18.6's is.

**A bare digit opens that slot.** `NormalCommand::Digit(1..=9)` joins
`dialogmode`'s vocabulary — never `0` (no slot `0` exists; `ctrl+0` is
`frame::slot_clear`) and never a modified digit (`ctrl+3` is the frame's
own regroup chord). In the Groupings browse list it opens that slot's
edit stage in one keystroke, unfilled slot or not; in another slot's
edit stage it jumps straight across, with no `escape` first. The open
slot's own digit answers `already editing slot N` rather than re-entering
the stage: re-entry rebuilds the draft from `services.config`, which can
be a `WRITE_DEBOUNCE` behind the last tick, so the stage would visibly
lose an edit that is in fact queued. Every other domain drops the digit
in browse (as it drops every key browse has no verb for) and names it in
the edit stage (`3 is not a verb here`, the edit stage's rule). Filter
mode is untouched: a digit typed there is text. Both footers advertise
`1`–`9` on Groupings only.

**`i` opens the chain field.** `NormalCommand::EditText` — reserved
since §3.1 for "a `Text` edited in place behind `i`" and until now a
notice on every draft — opens, on Groupings only, a field in the filter
row's place: the shared `Input`, labelled `slot N · chain`, seeded with
the slot's current chain in `GroupingSlots::label_of`'s own `book / lhu`
spelling so appending is a separator and a name away. The design:

- **The row list is the completion list.** While the field is open,
  `Draft::visible_rows` is `groupings::chain_candidates`: the
  `dimensions` items whose name matches the segment after the last
  separator, minus every name already typed before it, in row (schema)
  order — never a field header, since a header is nothing `tab` could
  complete to. The highlighted row is the completion; `j`/`k`, the
  arrows and the `ctrl` steps move it as in filter mode.
- **`/` and whitespace both separate** (user amendment, 2026-09-12),
  runs of either count as one, so `book lhu desk` and `book / lhu /
  desk` are the same chain. `tab` replaces the trailing segment with
  the highlighted candidate and opens the next with the canonical
  ` / `; the caret lands at the end because `InputState::set_value`
  puts it there on a single-line input (pinned checkout, `state.rs`
  `reset_selection`).
- **`enter` applies.** The typed names become the chain in typed order
  — ticked and first, every other item after, unticked — and the field
  closes; a `Step::Changed` then rides exactly the path a tick does
  (`revalidate`, `commit_or_confirm`), so a desk-owned slot still asks
  before forking and the write joins the same batch behind the same
  debounce. `Step::Refused` keeps the field open with the text intact
  and says why — an empty chain (§3.3's "must keep at least one entry"
  refusal, since the config model has no empty chain), a name twice
  (`'book' is listed twice`), or a name no dataset carries. `Step::Inert`
  (the same chain typed back) closes the field with nothing queued;
  closing is the visible answer.
- **`escape` cancels**: text dropped, field closed, chain untouched,
  stage still `Edit`. It is the field's own rung, ahead of the ladder,
  because a field whose text is a value must not walk `LeaveFilter` and
  leave the chain sitting in the filter.

**State shape.** `Draft::chain_entry: bool` beside `confirm`, not a
`Stage` — the escape ladder and the browse cursor restore key on
`Stage::Edit`, and both must still read `Edit` here. The chain text is
`Draft::query`: the field runs in `DialogMode::Filter`, which is what
hands the shared `Input` the keys through `dialog::sync_dialog_text`
(§16), and the `Change` subscription's one-way mirror into
`Draft::query` then holds the chain the way it holds a filter — no
second text buffer, and every transition (open, complete, apply, cancel)
stays a pure mutation of `chain_entry`/`query`/`mode` that the sync
settles on the handler's return. `handle_chain_key` is dispatched ahead
of the filter-mode branch, since the two share a focused `Input` and
`enter`/`tab`/`escape` mean different things in each. The pill reads
`chain` (`dialog::chain_pill`, the `primary` "you are typing" pair —
`filter` would misdescribe what `enter` does), through the same
`title_extra` slot §18.1 gave the mode pill; `mode_pill` and
`chain_pill` share one `state_pill`.

**Deliberately not built.** Views' column list gets no chain field: its
membership carries `kind` and a member/available split the one-line
grammar cannot spell, and nobody asked. The chain field does not open
on `enter` — `i` is the reserved key and `enter` keeps its notice.
Eleven harness entries guard the digit range, both domain gates, the
re-entry guard, the separator set, the typed order, the completed-name
exclusion, the duplicate refusal, the handler's dispatch order, the pill
and the row.
