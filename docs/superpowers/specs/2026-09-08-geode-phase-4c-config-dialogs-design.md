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

- `slot` — `Number`, 1–9.
- `name` — `Text`.
- `dimensions` — `OrderedList` over `pickable_columns(config)`, no
  per-item `width`. All `Doc`.

### 8.3 Sources

- `dataset` — `Choice`. `paths` — `Text`. `readiness`, `priority` —
  `Choice`. `poll_interval`, `pending_timeout` — `Text`, duration-parsed
  by the reader. `batch_pattern` — `Text`. All `Doc`.
- A write here opens the reload prompt of old 4c §5.7, unchanged:
  "Sources or datasets changed. Reload data now?", backed by the
  swappable `DataHandle` and a `DataService` restart on the background
  executor.

### 8.4 Scopes

Deliberately the thinnest, and the one narrowing of "full CRUD":

- `name` — `Text`. A read-only summary of what the scope selects.
- Action rows: `Load into frame`, `Replace with the current scope`,
  `Delete`, and `Revert to desk` when overridden.

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
`writable()` is `false` (§4), over `datasets` and `dimensions`: browse
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

- **A write that fails** (permissions, a full disk) shows its `io::Error`
  text on the object header row and leaves the draft intact so it can be
  retried. It is never silent, unlike the six existing persists, which
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
- a field edit writing immediately instead of staging into the draft
  (§3.2), which would fire the watcher mid-edit and reload a
  half-finished object;
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
  matches `path`** (§7.2 step 3, §8.5). `Diagnostic` has no `path` field
  yet — adding one touches all of its construction sites across
  `geode-core` plus every reader that would need to fill it in, none of
  which Part 1's tasks built. Validation still runs (§7.2 steps 1–2); its
  diagnostics render against the object header instead.
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
3. **Reverse stepping.** §3.3 steps both `Choice` and `Number` with
   `space` forward and `shift+space` back; `dialogmode::normal_command`
   (`dialogmode.rs:83-119`) has no `shift+space` case at all — its shift
   branch handles only `j`/`k`/`g` — so *every* steppable kind is
   forward-only today: `Choice` wraps forward with no way back, and
   `Number` steps forward only, clamping at `max` with no way back down.
   This is one missing key in the interaction model, not a per-kind gap,
   so the task is `shift+space` itself, with `Choice` and `Number` as its
   two consumers — implementing it for `Number` alone and leaving
   `Choice` still forward-only would satisfy the letter of a
   `Number`-only task name while missing half the defect. Whichever
   adapter builds the first real `Number` field — Groupings' `slot`
   (§8.2) is the obvious candidate — would otherwise inherit a field
   that can be raised and never lowered. Views exercises `Choice` but
   never needs to step it backward, and has no `Number` field at all, so
   nothing in Part 1 forced either half into view.

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

Its cost has risen since the sketch. `bridge.rs:209` discards the
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
