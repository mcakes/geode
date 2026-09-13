# Geode Phase 4c Part 2c — Column Presentation and Named Colours

Approved in design 2026-09-13. Governs the last part of Phase 4c: a
trader's own per-column presentation (label, width, scale, precision,
thousands, negative, colour) edited in a column stage under a view's
member row and carried by `view_presentation.toml` without forking the
desk's view; and **named colours**, a config doc of shared colours that
resolve against the active theme, edited in a dialog of their own and
painted by the blotter now and by charting later.

Background: `docs/superpowers/specs/2026-09-08-geode-phase-4c-config-dialogs-design.md`
(the scaffold, §3–§7; the Views adapter, §8.1 and §16–§19.8; §20 is the
direction note this spec replaces), Phase 3 §6.2 (`ColumnFormat` and the
desk's `format` table), and the interaction-model spec's §16 (the pure
state is the truth; `dialog::sync_dialog_text` is the only focus and
text writer). Three rulings taken in design and binding here: a shared
colour is a **hue on a canonical wheel that each theme transforms**, never
a literal hex; a named colour paints a column's **header and values**, and
does not combine with `sign`; the overlay carries per-column keys in
**one `[view.columns.<col>]` table per column**.

## 1. Scope

### 1.1 What Part 2c delivers

1. `geode_core::colour`: the `colours` doc reader, the `Definition` type,
   and a pure resolver from a definition and a theme's anchors to an sRGB
   colour, with the OKLab/OKLCH conversion it needs.
2. `Colour::Named(String)` on a column's format; the views reader accepts
   it and `load_views` warns, with the column's path, on a name no
   colour doc defines.
3. The overlay reshape: `view_presentation.toml` gains
   `[view.columns.<col>]` tables carrying every presentation key; the
   legacy `hidden` array and `width` map still load; the writer emits
   only the new spelling and only keys that differ from the desk.
4. The **column stage** in the Views dialog: `enter` on a member row opens
   seven presentation fields for that column, every one
   `Destination::Presentation`.
5. The **Colours dialog** (`Domain::Colours`, `config::colours`): browse
   with a live swatch per row, an edit stage of hue, tone and token with
   a live swatch in the header, create, delete, revert, fork and drift as
   on every domain.
6. Blotter painting: a named colour paints the column's header label and
   its additive cell values, resolved at paint time against the active
   theme.
7. Theme checks: a test over every bundled theme reporting its smallest
   anchor arc and asserting every generated hue clears 3:1 against the
   theme's background.

### 1.2 Done state

- A trader opens `config::views`, opens `tree`, presses `enter` on
  `npv`, steps `Scale` to `k` and `Precision` to `0`, and the blotter
  paints `npv (k)` with no decimals a beat later, while
  `view_presentation.toml` gained exactly
  `[tree.columns.npv]\nscale = "k"\nprecision = 0` and `views.toml` is
  untouched.
- The same trader opens `config::colours`, presses `n`, names it
  `delta`, steps `Hue` to 240 and sees the header swatch turn the active
  theme's blue; switching theme (`ctrl+k`, a theme row) turns the swatch
  and every `delta`-coloured column to the new theme's blue on the next
  frame.
- `Colour` on `delta01`'s column stage offers `none`, `sign`, `delta`;
  choosing `delta` paints the header and values, and `views.toml` is
  still untouched.
- A `colours.toml` entry with both `hue` and `token` is dropped with an
  error naming its path; a column naming a colour that does not exist
  warns at load with `views.<view>.columns.<i>.format.colour` and paints
  in foreground.
- Every bundled theme passes the anchor-arc and contrast test.

### 1.3 Explicitly not in Part 2c

- The blotter's header drag does not write a width back (unchanged from
  2b).
- No diff, no per-theme sidecar of hand-tuned colours (§2.5 leaves the
  door open), no literal hex colours.
- Charting does not exist yet; this spec only guarantees that a chart
  can resolve a named colour the same way the blotter does.
- Renaming an object stays unbuilt (4c §8.2 ruling).

## 2. Named colours: the model

### 2.1 The doc

`colours.toml`, atomic at depth one (`config::merge::atomic_depth`), one
table per named colour. A definition is exactly one of two shapes:

```toml
[delta]
hue = 240            # 0..360 on the canonical wheel

[gamma]
hue = 210
tone = "light"       # "normal" (default) or "light"

[pnl]
token = "chart.bullish"
```

A table with both `hue` and `token`, or neither, is an error and the
colour is dropped; `tone` beside `token` is a warning and ignored; a hue
outside `0..360` is an error (360 is 0). Names go through
`check_object_name`; `none` and `sign` are refused, because a column's
`colour` key already spells those two.

```rust
// geode_core::colour
pub enum Tone { Normal, Light }
pub enum Definition {
    Hue { degrees: f32, tone: Tone },
    Token(Token),
}
pub struct NamedColours { by_name: BTreeMap<String, Definition> }
impl NamedColours {
    pub fn from_doc(doc: &MergedDoc) -> (NamedColours, Vec<Diagnostic>);
    pub fn get(&self, name: &str) -> Option<&Definition>;
    pub fn names(&self) -> impl Iterator<Item = &str>;   // sorted
}
```

Diagnostics carry paths in 2b's grammar: `colours.<name>`,
`colours.<name>.hue`, `.tone`, `.token`.

### 2.2 The wheel and the anchors

The canonical wheel places six hues at fixed angles: red 0, yellow 60,
green 120, cyan 180, blue 240, magenta 300, wrapping at 360. Every
bundled theme sets twelve base colours — those six, each in a normal and
a light tone (`base.red`, `base.red.light`, …, the component library's
`red`/`red_light` … `magenta`/`magenta_light` fields). Those are the
**anchors**, and a theme is nothing more to this model than where it puts
them.

```rust
pub struct Rgb { pub r: f32, pub g: f32, pub b: f32 }   // sRGB, 0..1
pub struct Anchors { pub normal: [Rgb; 6], pub light: [Rgb; 6] }  // red, yellow, green, cyan, blue, magenta
pub struct Tokens { pub foreground: Rgb, /* … one field per Token variant … */ }
pub fn resolve(def: &Definition, anchors: &Anchors, tokens: &Tokens) -> Rgb;
```

**Amended (as built, 2026-09-13).** `Tokens` also carries
`pub background: Rgb` — the surface the readability floor below measures
against. It is deliberately **not** a `Token` a `colours.toml` definition
can name: no colour resolves *to* the background.

A hue resolves by:

1. finding the two anchors that bracket it (`floor(h / 60)` and the next,
   wrapping), and `t = (h mod 60) / 60`;
2. taking the theme's colours for those two anchors in the requested
   tone;
3. converting both to OKLCH and interpolating lightness and chroma
   linearly and hue along the **shorter arc** — an OKLab chord would pass
   through lower chroma and grey the midpoint, and an HSL midpoint of a
   theme's yellow and blue is a mud of the wrong lightness;
4. converting back to sRGB, and when the result leaves the gamut,
   pulling **chroma** in until it fits rather than clamping channels, so
   the hue and lightness the trader asked for survive.

An anchor hue (`t == 0`) resolves to the theme's own colour exactly, so
`hue = 240` IS that theme's blue and a trader who wants the theme's
palette can have it by name of angle.

**Amended (as built, 2026-09-13 — user ruling, Task 7).** That identity
rule is conditional: it holds **unless the theme's own colour is
unreadable on the theme's background**, in which case `resolve` moves
only its OKLCH *lightness* toward the foreground's — hue and chroma kept,
re-clipped to gamut — by the smallest amount (16-step bisection) that
brings `contrast_ratio` up to `READABLE_RATIO` (3.0). That is
`readable_on` in `crates/geode-core/src/colour/mod.rs`, and it applies to
`Definition::Hue` **only**: a `Definition::Token` is never floored, since
a token names one of the theme author's own deliberate semantic colours
rather than a generated point on the wheel. `interpolate_hue` itself is
untouched and stays unfloored, so the arc and anchor rules above describe
it exactly. The ruling exists because §7's check found the unfloored
wheel unreadable on 27 of 44 bundled themes (see §9); the floor is what
lets §7 assert both tones on every bundled theme with no exception list.
The design finding behind it, stated plainly: **§2.2's light tone is not
usable as a text colour on light themes without the floor** — a theme's
`.light` anchors are tints, authored as fills and accents, not as text.

`Tone::Light` interpolates along the light anchors. That is the whole
second dial: a family can have a normal and an emphasised member.

### 2.3 Tokens

`token` names one of a fixed vocabulary of theme colours the component
library already defines, so the semantic colours a theme ships — and the
pair sign colouring already uses — share one vocabulary with named
colours:

| token | theme field |
|---|---|
| `foreground` | `foreground` |
| `muted` | `muted_foreground` |
| `primary`, `accent`, `danger`, `warning`, `success`, `info` | the same-named fields |
| `chart.1` … `chart.5` | `chart_1` … `chart_5` |
| `chart.bullish`, `chart.bearish` | `chart_bullish`, `chart_bearish` |

An unknown token is an error and the colour is dropped. `Tokens` is a
plain struct of `Rgb` the caller fills from the theme; `geode-core` never
sees a gpui type.

### 2.4 The conversion

`sRGB → linear → OKLab → OKLCH` and back, in `geode_core::colour::oklab`,
about forty lines of pure arithmetic with no dependency, unit-tested
against published reference values (white, the primaries, mid grey) to
four decimal places, plus round-trip tests on a grid of sRGB values.

### 2.5 Known strains

- **Folded arcs.** A theme whose neighbouring anchors sit close together
  (a red and a magenta both near 350°) folds that sixty-degree span, so
  two definitions far apart on the canonical wheel can nearly coincide
  there. Inherent to any theme-relative scheme; §7's checks report it
  per theme rather than a rule forbidding it.
- **Distinguishability is per theme.** Two colours twenty degrees apart
  separate on a saturated theme and blur on a muted one. The dialog's
  live swatch is the defence.
- **A theme could be hand-tuned later** with a sidecar listing slot
  colours; nothing here precludes it, and nothing here builds it.

## 3. A column's colour

`geode_core::view::Colour` gains a third variant:

```rust
pub enum Colour { None, Sign, Named(String) }
```

The views reader accepts `colour = "<name>"` for any string other than
`none` and `sign`; `ColumnFormat.colour` carries it. `config::load_views`
— which already merges the overlay over the views and is the one place
with the whole `Config` in hand — cross-checks every `Named` against the
`colours` doc and pushes a warning with the column's own path
(`views.<view>.columns.<i>.format.colour`, or the overlay's
`view_presentation.<view>.columns.<col>.colour`) for a name it lacks; the
column keeps the name and the blotter paints it in foreground (§6.3).

A named colour paints the column's **header label and every additive
cell value** (ruling). `sign` is unchanged. A column is one of `none`,
`sign` or a name; there is no combination, and a trader who wants
negatives red inside a blue family asks for a second key in a later
phase, not a rule here.

## 4. The overlay reshape

### 4.1 Shape

`view_presentation.toml` carries, per view, an `order` array as today
and one table per personalised column:

```toml
[tree]
order = ["npv", "delta01", "book"]

[tree.columns.npv]
scale = "k"
precision = 0
width = 120

[tree.columns.delta01]
colour = "delta"
label = "Δ01"

[tree.columns.gamma01]
hidden = true
```

The keys are exactly `ColumnPresentation`'s: `precision`, `thousands`,
`negative`, `colour`, `scale`, `label`, `width`, `hidden`, each optional,
each read by the same code the desk's `format` table and column keys
use, so the two spellings cannot drift.

```rust
pub struct ViewPresentation {
    pub order: Vec<String>,
    pub columns: BTreeMap<String, ColumnPresentation>,   // replaces `hidden` and `width`
}
```

### 4.2 Reading both spellings

The reader still accepts the legacy `hidden = [..]` array and
`width = { col = n }` map, folding each into `columns.<col>.hidden` /
`.width`. A column named in both a legacy key and its own table takes
the **table's** value and warns (`view_presentation.<view>.columns.<col>.width`:
"also set in the legacy 'width' map — the table wins"). This is what
lets a file written by Part 2b load unchanged.

### 4.3 Writing only what differs

The writer (`views::presentation_table`) emits `order` when it differs
from the doc's own order (as today) and `[view.columns.<col>]` with
**only the keys whose value differs from the column's desk baseline**,
where the baseline is the kind's default (`ColumnFormat::MEASURE` /
`TEXT`) with the desk's `format` and `label` applied and `width` as the
desk's column carries it. A key equal to the baseline is not written;
a column with no differing key gets no table. This is the property that
keeps the overlay a *personalisation*: a trader who changes a precision
does not copy the desk's scale into their file and freeze it against the
desk's next change. `hidden` is user-only and written whenever set, as
today. The writer never emits the legacy keys, so a file migrates itself
the first time its view is written.

### 4.4 Applying

`ViewPresentation::apply` merges each set key of `columns.<col>` over the
view's `presentation` entry for that column (the `Option` merge
`ColumnFormat::with` already performs), warning with a path for a column
the view lacks, as it does today for `hidden` and `width`.

## 5. The column stage

### 5.1 The item carries the whole presentation

```rust
pub struct ListItem {
    pub name: String,
    pub included: bool,
    pub presentation: ColumnPresentation,   // replaces `width: Option<f32>`
    pub kind: Option<String>,
}
```

`presentation` is the column's presentation **as the trader sees it**:
the kind default, the desk's `format` and `label` over it, the overlay
over that — what `load_views` produced. The member row paints a compact
summary after the name of the keys in force that differ from the kind
default: `npv · 120 px · k · 0 dp · delta`.

### 5.2 Entering and leaving

`enter` on an `EditRow::Item` opens
`Stage::Column { object: String, column: String }`; on an `Available` row
it keeps 2b's notice. The stage is a projection over the same `Draft`:

```rust
impl Draft {
    pub fn enter_column(&mut self, column: &str, colours: &[String]) -> bool;  // stashes fields, installs the column's
    pub fn leave_column(&mut self);                                            // folds back, restores, cursor on the column
    pub fn fold_column(&mut self);                                             // column fields → the item's presentation
    pub fn column(&self) -> Option<&str>;
}
```

`enter_column` stashes the view's fields in `parent_fields`, installs the
seven fields below, resets `query` and the cursor, and reports whether
the name was a member. Every `Step::Changed` in the stage runs
`fold_column` before `revalidate` and `commit_or_confirm`, so the batch
renders the overlay from the item on the same keystroke. `escape` walks
filter → clear → `leave_column`, which restores the view's fields with
the cursor on the column's row; `has_previous_stage` is true in the
stage. The crumb reads `tree › npv`; the title row pill and the filter
row are the edit stage's. `is_dirty` and `mark_saved` compare the
installed fields as they do any other, so a keystroke in the stage is
accounted for exactly like a tick.

**Amended (as built).** The three signatures landed as
`enter_column(&mut self, column: &str, fields: Vec<Field>) -> bool` (the
adapter builds the fields — `views::column_fields(item, colours)` — and
hands them in, so the pure core never reaches for a domain's vocabulary),
and `fold_column(&mut self) -> Option<&'static str>`, whose answer is the
key that fell back to the desk (§5.3's amendment); `leave_column` and
`column` are as written. `field_by_key` — installed fields first, then
the stashed `parent_fields` — is the load-bearing addition the sketch
does not have: `list_items`, `available_items` and `choice` all route
through it, because the write path renders the *whole object* on every
keystroke and without the fallback a keystroke in the column stage would
render a view with no columns at all.

### 5.3 The seven fields

Every one is `Destination::Presentation`, so nothing here can fork and
nothing asks. Keys match the overlay's:

| Field | Kind | Rule |
|---|---|---|
| `label` | `Text`, editable | empty means the column's own name; `parse_text` trims |
| `width` | `Text`, editable | `auto` or an integer 20..=2000, refused otherwise; a `Text` because `auto` is a value |
| `scale` | `Choice` | `none`, `k`, `M` |
| `precision` | `Number` | 0..=12, step 1 |
| `thousands` | `Bool` | |
| `negative` | `Choice` | `minus`, `parens` |
| `colour` | `Choice` | `none`, `sign`, then every named colour sorted — read from the `colours` doc when the stage opens |

`Domain::text_editable` answers `true` for `label` and `width` on Views
while a column stage is open; `parse_text` on Views handles those two
keys. A `Number`'s step is the new `FieldKind::Number.step` (§5.4).

**Amended (as built — ruling, Task 4).** "Empty means the column's own
name" is too narrow, and `auto` needs the same sentence. Both cleared
states mean **"as the desk has it"**: `views::fold_into` takes the desk
baseline and resolves an empty `label` to `desk.label` and an `auto`
`width` to `desk.width` (`None` where the desk sets neither, which is
where the original wording's "the column's own name" and a
component-chosen width come back), so `presentation_table` finds equality
and writes no key at all — which is precisely "this trader has no opinion
here". `fold_column` then **re-seeds** the installed `label`/`width`
field from the item, so the desk's own value is on screen on the same
keystroke rather than a blank the next rebuild contradicts, and returns
the key that fell back for the notice `"<key> follows the desk again"`.
The consequence, and §4.3's rule standing rather than being worked
around: a trader cannot *delete* a desk-declared label from this dialog,
only stop overriding it — nor pin a key at the value the desk currently
has, since an equal key is never written. That is the overlay's
semantics, not a gap.

### 5.4 `FieldKind::Number.step`

```rust
Number { value: i64, min: i64, max: i64, step: i64 }
```

`space`/`shift+space` move by `step`. 2b's refuse-don't-clamp rule holds
for a value already outside `min..=max`: it is refused, never moved. A
value inside the range that a step would carry past a bound lands on the
bound (`355 + 15` on `0..=359` lands on 359) — unless the field wraps:
`Number` gains `wrap: bool`, true for hue alone, and `350 + 15` on a
wrapping hue lands on 5, since a hue is circular. Every existing `Number`
keeps `step: 1, wrap: false`.

### 5.5 Diagnostics in the stage

A path `views.<view>.columns.<i>.format.<key>` (or `.label`, `.width`)
whose index resolves — by name, 2b's rule — to the open column lands on
the row keyed `<key>`; any other path stays on the header, and a path
naming a different column lands on nothing while this stage is open.

## 6. The Colours dialog and the blotter

### 6.1 `Domain::Colours`

Palette `config::colours`, "Edit colours", category Configuration, no
default binding. Doc `colours`; no presentation doc; roster `None`;
`writable()` true.

**Browse.** One row per colour sorted by name, the usual layer,
overridden and drifted badges, a summary of `hue 240`,
`hue 210 · light` or `token chart.bullish`, and a **swatch**: a small
filled square (`px(14)`, rounded like a badge) painted in the colour
resolved against the active theme — the shell reads `cx.theme()`'s
anchors and tokens into `Anchors`/`Tokens` once per render and calls the
core resolver. Selector `objectdialog-swatch-<name>`.

**Edit.** Three fields, all `Doc`:

| Field | Kind | Rule |
|---|---|---|
| `hue` | `Number` 0..=359, step 15, wrap | `i` types an exact value |
| `tone` | `Choice` | `normal`, `light` |
| `token` | `Choice` | `none` first, then the fifteen tokens of §2.3 |

`none` means "use the hue and tone"; any token overrides them.
`to_table` writes only the keys in force (`token`, or `hue` plus `tone`
when light), so the file never holds a dead hue beside a token. The
header carries the swatch, repainted on every step, which is what makes
stepping the hue a live tuning gesture. `validate` runs
`NamedColours::from_doc` over the rendered object alone.

`n` creates `{ hue = 0 }` and opens the stage; `check_object_name` plus
the two reserved names gate it. `d`, `r`, the fork confirm and 2b's
overrides entry work as on every domain.

### 6.2 Definitions travel like views

`data_setup` and the bridge's `ConfigReloaded` arm read the `colours` doc
beside `views` and hand `NamedColours` to the blotter factory
(`factory.set_colours`), exactly as they hand `ViewSpec`s;
`hot_reload::apply_reload` fires `ConfigReloaded` for a `colours` change
as it does for `views`. A tile holds an `Arc<NamedColours>` from its
factory. Nothing new touches `ShellView`, and the one gpui global stays
`UiSettings`.

### 6.3 Resolution at paint

`PlannedColumn.format.colour` already reaches the delegate. For a
`Colour::Named`, the delegate resolves the colour against `cx.theme()`
once per frame per column through the core resolver, behind a cache
keyed on the twelve anchor values and the fifteen token values read
from the theme: a steady theme costs one comparison a frame, and a theme
switch repaints on its next frame with no event.

- An additive cell paints its text in the colour; a determined
  non-additive cell stays muted; a non-attributable cell stays blank.
- The header label paints in the colour through `BlotterDelegate`'s own
  `render_th` (the pinned component's default renders the column's
  plain name).
- A name the doc lacks resolves to `None` and the column paints in
  foreground; the load-time warning (§3) already said why.

## 7. Checks and the harness

- **Theme checks** (`theme.rs` tests, over every bundled theme): for each
  theme, read its anchors, report the smallest OKLCH hue arc between
  neighbouring anchors, and assert that every hue at 30-degree steps in
  both tones clears a 3:1 contrast ratio against the theme's
  `background`. A theme that fails is a theme-authoring defect named by
  the test, not a runtime surprise.

  **Amended (as built, 2026-09-13).** The assertion reads through
  `resolve`, so §2.2's readability floor is in force: every bundled entry
  clears 3:1 on all 12 hues in **both** tones, with **no exception list**
  (`every_bundled_theme_keeps_generated_hues_readable`, `theme.rs`). The
  original sentence's "a failing theme is a theme-authoring defect named
  by the test" is therefore not the failure mode any more — nothing
  fails. What the test *reports* instead is the per-theme **floor count**:
  how many of that theme's 24 hue/tone pairs `readable_on` had to move,
  printed as the retune work order. The counts are in §9.
- **Resolver tests**: an anchor hue is the theme's own colour; a hue
  between anchors interpolates along the shorter arc (350 → 10 passes
  through red, not through cyan); tone selects the light anchors; a
  gamut overflow pulls chroma, not lightness; a token resolves to the
  token field.
- **Harness entries**, one per behaviour: the reserved names; both-or-
  neither refused; the wrap in `resolve`; the shorter arc; chroma
  clipping; the legacy `hidden`/`width` folding; the table winning over a
  legacy key; the differs-from-desk writer omitting an equal key; the
  column stage's fold-back running before the commit; `enter` on an
  available row refused; `Number.step` and `wrap`; `to_table` omitting a
  dead hue under a token; the unknown-name warning's path; the cache
  invalidating on a changed anchor; `ConfigReloaded` on a `colours`
  change.

## 8. Sequencing

One branch, eight tasks, each resting on the one before:

1. `geode_core::colour`: OKLab/OKLCH, `Definition`, `Anchors`/`Tokens`,
   `resolve`, `NamedColours::from_doc`, `merge.rs` registration;
   `Colour::Named`, the views reader, `load_views`' cross-check.
2. The overlay reshape (§4): `ViewPresentation.columns`, both spellings
   read, the writer, `apply`.
3. `Number.step`/`wrap`; `ListItem.presentation`; the member-row summary.
4. The column stage (§5).
5. The Colours dialog (§6.1), its swatches and action.
6. The bridge, factory and blotter painting (§6.2–§6.3).
7. The theme checks (§7).
8. Docs (`CLAUDE.md`, an "as built" section here, the 4c spec's §20
   pointer), the harness reviewed as a set, the full harness.

Display checks pending on the user's screen, as every Phase 4c branch has
recorded: the swatches, the header colour, the member-row summary, the
column stage's crumb.

## 9. As built

Part 2c shipped as eight tasks on `worktree-phase-4c-part-2c`
(`bba40a5`..`26d2bc6` on top of `c6d6c1f`), 32 harness entries added
(656 → 688), ten entries re-anchored where these tasks moved their
source lines (eight predating the branch, two added earlier on it) and
one re-pointed at a covering test that can actually see it. §2.2, §5.2,
§5.3 and §7 above
are amended in place where the build or a ruling contradicted them;
§4.3's rule stands unchanged, and §4.3's own impossibility (a key cannot
be pinned at the value the desk currently has) is recorded as a ruling
below rather than treated as a defect. What follows is what a maintainer
has to know that the design did not say, task by task, then the rulings,
the deferred minors, the theme numbers and the display-pending list.

### 9.1 Task 1 — `geode_core::colour`, `Colour::Named`, the cross-check

`crates/geode-core/src/colour/oklab.rs` is Ottosson's published matrices
transcribed verbatim, with `#[allow(clippy::excessive_precision)]` on the
two conversions and a comment saying why truncating them would drift the
constants off the reference values the tests check against.
`to_srgb_in_gamut` is a 16-step bisection pulling **chroma** in, never
lightness or hue. `colour/mod.rs` holds `Rgb`, `Tone`, `Token` (15
variants, `parse`/`name` round-trip, `Token::ALL`), `Definition` with
`summary()`, `RESERVED_NAMES = ["none", "sign"]`, `NamedColours`,
`ANCHOR_DEGREES`, `Anchors`, `Tokens`, `interpolate_hue`, `resolve` and
`contrast_ratio`.

- **`Colour` and `ColumnFormat` lost `Copy`**, because `Named` carries a
  `String`. Two call sites were patched then: `ColumnFormat::with` clones
  `p.colour`, and the blotter delegate's per-column read. Task 6 removed
  the second one again — see §9.6.
- **The views reader accepts *any* string that is not `none` or `sign`
  as a name**, because the doc it would need to check against is not in
  scope there. `config::load_views` — the one place with the whole
  `Config` in hand — does the cross-check, **before** the presentation
  overlay's `apply`, so the diagnostic's column index is the file's own
  order rather than the overlay-reordered one. A name no doc defines is a
  warning, the column keeps the name, and the blotter paints it in
  foreground (§6.3).
- **`load_views` throws `NamedColours::from_doc`'s own diagnostics away**
  — it reads that doc only to cross-check names. The bridge is the only
  place a malformed `colours.toml` is ever reported (§9.6).
- A pre-existing reader test's fixture changed from `colour = "loud"` to
  `colour = 42`: with `Named` in place, `"loud"` is a well-formed name,
  so the "bad colour value" warning arm needed a non-string value to
  reach it.
- **A harness gap found and closed rather than reported.** The
  shorter-arc entry first `SURVIVED`: the six saturated sRGB
  primaries/secondaries in the test fixture are not evenly spaced 60°
  apart in real OKLab hue (magenta sits ~58° from red), so the existing
  comparative assertion ("closer to red than magenta is") still held
  under the mutation. One additive absolute bound (`arc(got.h, red.h) <
  0.3`; 0.168 rad unmutated, 0.879 mutated) made the entry honest.

### 9.2 Task 2 — the overlay reshape

`ColumnPresentation::parse_format_keys`/`parse_column_keys` are the one
parse the views reader and the overlay both call, which is what keeps the
two spellings from drifting. `ViewPresentation` is `{ order, columns:
BTreeMap<String, ColumnPresentation> }`; `from_doc` reads the
`[view.columns.<col>]` tables first and then folds the legacy `hidden`
array and `width` map into the same map, the table winning a conflict
with a warning. `ColumnPresentation::merge_over` folds one column's
overlay over the view's own, and `apply` calls it per column.

- **`warn: &dyn Fn(&str, String)` forced a `RefCell`.** The shared
  callback is an `Fn`, not an `FnMut`, so it cannot push into a captured
  `&mut Vec<Diagnostic>`; both call sites collect into a local
  `RefCell<Vec<Diagnostic>>` for the duration of the two `parse_*` calls
  and `extend` immediately after, preserving diagnostic order exactly.
- **`merge_over`'s per-field guards are rustfmt's three-line form**, not
  a one-liner: `cargo fmt --check` is a CI gate and rustfmt always
  expands `if cond { stmt; }`. Each field is still its own unique anchor.
- **An anchor-substring collision caught before it could lie.** The
  overlay-side colour check, first written in the same let-chain shape as
  the view-side one, made the *shorter* (less-indented) Task 1 anchor a
  literal substring of the deeper one — two matches, so `--anchors-only`
  would have reported the merged Task 1 entry ambiguous. The overlay
  check was rewritten with a `let … else { continue }` and a plain `if
  colours.get(name).is_none() {`, sharing no suffix with the other.
- One harness entry mutates the `scale` guard rather than the `precision`
  one the plan sketched, because the covering test's own fixture sets
  `precision` on both sides — the `precision` guard is unobservable
  there, and the entry would have been caught for the wrong reason or not
  at all.

### 9.3 Task 3 — `Number.step`/`wrap`, `ListItem.presentation`, the writer

`FieldKind::Number` gained `step: i64` and `wrap: bool`;
`step_selected`'s `Number` arm is one wrap-aware formula (`span = max -
min + 1`, wrap via `(next - min).rem_euclid(span) + min`, otherwise clamp
to the bound) replacing the old per-direction early returns, with 2b's
out-of-range refusal in front of it. `ListItem.width: Option<f32>` became
`ListItem.presentation: ColumnPresentation`. `views::presentation_table`
is the differing-keys writer; `views::kind_default`/`column_summary`
produce the member row's compact summary (`120 px · k · 0 dp · delta`),
painted muted after the name, replacing the old right-hand `{width}px`.

- **`desk_baseline` reads `draft.source` directly, and must keep doing
  so.** Building it from `columns_for`'s `toml_edit::ArrayOfTables` via
  `Table::to_string()` silently drops every column's nested `format`
  sub-table — a bare `toml_edit::Table` with no document to hang a header
  path off prints only its own flat pairs — so the writer thought every
  format key differed and copied the desk's into the trader's file. Its
  doc comment records the trap.
- **`#[allow(clippy::collapsible_if)]` on `presentation_table` is
  deliberate**: clippy's suggested `if … && let Some(v) = …` collapse
  folds two lines that separate harness entries anchor independently.
- **`column_summary`'s `thousands` check was one-directional** and said
  nothing when a *dimension* turned thousands on; found while writing the
  test the review's Important 2 asked for, and fixed to compare both
  directions (`thousands` / `no thousands`).
- **One harness entry was caught for the wrong reason and fixed.**
  `views: a presentation save copies the desk's widths into the user's
  file` mutated its guard to `if false`, which *deletes* the width write
  rather than reproducing the copy it names; the covering test only
  noticed because the trader's own width vanished. It now mutates to `if
  true` (an unconditional write, which really does copy the desk's
  matching width) under
  `a_presentation_save_writes_only_what_the_trader_changed`, whose desk
  view declares `width = 140` and asserts it never appears.
- `ListItem.included` is the one truth about membership from construction
  on; `presentation.hidden` is only the seed `views::fields` read out of
  the merged overlay, never consulted again. Both docs say so.

### 9.4 Task 4 — the column stage

`Stage::Column { object, column }` is a **projection over the same
`Draft`**, not a draft of its own: `has_previous_stage`, `set_query`'s
one-way mirror and `effective_query` all take the side `Stage::Edit`
does. `Draft` gained private `parent_fields: Option<Vec<Field>>` and
`column: Option<String>`; `views::COLUMN_KEYS` is the statement of record
for the seven rows and `column_fields` `debug_assert!`s its output
against it. Every `Step::Changed` in the stage runs `fold_column` before
`revalidate` and `commit_or_confirm`.

- **The maintainer trap: while a column stage is open, `draft.fields` are
  the seven presentation fields, not the view's.** A consumer that reads
  `draft.fields` for the view's own list (`columns`, `dataset`) sees the
  column's rows instead. `Draft::field_by_key` — installed fields, then
  the stashed `parent_fields` — is the fallback every such reader
  (`list_items`, `available_items`, `choice`) goes through, and it is
  load-bearing rather than tidy: the write path renders the whole object
  on every keystroke, so without it one keystroke in the stage would
  render a view with no columns at all. The two key sets are disjoint
  (`COLUMN_KEYS` vs `dataset`/`columns`), so nothing shadows.
- **`row_for_path` is scoped to the open column.** A
  `columns.<i>.[format.]<key>` path resolves its index **by name** (2b's
  rule) against the parent's items; a path naming a different column
  lands on nothing while the stage is open.
- **Three fixes the stage made reachable, all beyond the brief and all
  kept:** `presentation_table`'s desk comparison resolves both sides
  through `kind_default` (the ruling below); `apply::revert_failed_write`
  steps a `Stage::Column` back to `Stage::Edit`, since the draft it
  rebuilds carries no projection, and resolves the cursor by NAME through
  a shared `Draft::select_item_named` that `leave_column` also calls; and
  `edit_commit_notice` gained a third answer, "press i to type a value",
  for a `Number` or an editable `Text` — which also corrects the same lie
  on every Sources text row, where it predates this task.
- **`enter` goes through one door in both modes.** `commit_selected_row`
  is called from the normal-mode `Commit` arm *and* filter mode's
  `enter`; the edit stage's filter-mode `enter` used to give the commit
  notice, justified by "there is nothing here to open", a reason this
  task expired. It is how a trader reaches one column of a thirty-column
  view.
- `x`, `shift+j` and `shift+k` answer `"<key> is not a verb in a column's
  stage"` (one sentence, the pressed key filling the placeholder), gated
  on `draft.column().is_some()`. `leave_column` clears `confirm` — today
  unreachable (the armed block claims `escape` before that rung is read),
  which is exactly why it is cleared rather than relied on.
- `MoveItem` is the one arm that commits without revalidating, and so
  without folding; it cannot fire in the stage, since every row there is
  an `EditRow::Field` and `Draft::move_item` answers `None` for one. A
  future item row in the stage would change that.

### 9.5 Task 5 — the Colours dialog

`objectdialog/colours.rs` is the adapter (`DOC`, `summary` routed through
`NamedColours::from_doc` over a one-entry doc, `fields`, `definition_of`
for the live swatch, `to_table`, `validate`); `shell/colours.rs` is the
gpui↔pure bridge (`to_rgb`, `to_hsla`, `anchors_from_theme`,
`tokens_from_theme`, `resolve_named`). `dialog::swatch` is a 14px rounded
square whose border is always `theme.border`, never the resolved fill.
Selectors: `objectdialog-swatch-{name}` per browse row and
`objectdialog-swatch-header` in the edit header.
`Domain::reserved_names()` answers `RESERVED_NAMES` for Colours and `&[]`
elsewhere, and `name_taken` checks it first, so `n` refuses `none`/`sign`
with `"'<name>' is reserved"` ahead of the listed and presentation-only
branches.

- **Two files are named `colours.rs`** — the adapter
  (`objectdialog/colours.rs`) and the theme bridge (`shell/colours.rs`) —
  disambiguated at the one place both are imported by an alias (`use
  super::super::colours as colour_theme;`).
- **`text_editable`/`parse_text` fold Colours into the existing
  `Groupings | Scopes | Schema` arm** rather than carrying a
  byte-identical duplicate: Colours has no `Text` row.
- **A harness entry survived and the test was strengthened, not the
  anchor swapped.** `colours: to_table omits the hue under a token` was
  unobservable against the original fixture, whose `source` never carried
  a `hue` key for the removal to remove; the test now switches a draft
  that *does* carry `hue = 210` / `tone = "light"` to a real token and
  asserts both stale keys are dropped.
- The browse list builds its `NamedColours` **once per list render**, and
  only when `state.domain == Domain::Colours`, so no other dialog pays
  for reading a doc it never uses.

### 9.6 Task 6 — definitions travel, the blotter paints

`hot_reload::views_changed` gained `|| changed("colours")`, so a
colours-only edit fires `ShellEvent::ConfigReloaded`. `DataSetup` carries
a `NamedColours`, read in `data_setup` from `config.doc("colours")` —
**with `NamedColours::from_doc`'s own diagnostics extended into
`setup.diagnostics`, the only place a malformed `colours.toml` is ever
reported** (§9.1). The `ConfigReloaded` arm re-reads the doc, logs its
diagnostics at `geode::query` warn beside the presentation ones, calls
`BlotterFactory::set_colours`, and folds both diagnostic sets into one
`note_data_diagnostics` batch. `BlotterFactory` holds
`Rc<RefCell<Arc<NamedColours>>>` shared with every tile exactly as `views`
is; `set_colours` installs a **fresh `Arc`**, and the tile hands
`Arc::clone` to the delegate in `apply` — the one place a plan is built
or rebuilt — so a delegate can never paint a plan against colours older
than it.

- **The cache is pure and keyed on the theme; the *definitions* are
  invalidated separately.** `ColourCache::get(colours, name, anchors,
  tokens)` takes `Anchors`/`Tokens`, never a `Theme` (the preflight
  ruling below), and clears itself when `(Anchors, Tokens)` changes — so
  a theme switch repaints on the next frame with no event. That alone
  would paint a **redefined** colour stale forever under an unchanged
  theme, so `BlotterDelegate::set_colours` compares the `Arc` **pointer**
  and calls `ColourCache::invalidate` on a real change. Both halves have
  their own test.
- **Two performance decisions beyond the design sketch, both for the
  render thread.** The per-cell `colour` read was a `Colour` clone — with
  `Named(String)` that is a heap allocation *per cell per frame*; it is
  now a `Copy` classification, `ColourKind::{Plain, Sign, Named}`
  (`colour_kind`). And `anchors_from_theme`/`tokens_from_theme` are read
  **inside** the named arm rather than at the top of `render_td`, so a
  blotter whose columns name no colour — every one of them today — pays
  nothing.
- **`render_th` is overridden** for the header colour: the pinned
  component's default renders the plain name. The sort icon, padding,
  borders, click handler and drag source all live in
  `TableState::render_th` *around* the delegate's, so nothing is lost.
- `bridge.rs`'s `stale_after` comment, which enumerated the
  `ConfigReloaded` trigger set as "views/dimensions only", was already
  stale for `view_presentation` and now names all four docs.
- There is no end-to-end "reload → live tile repaints in the new colour"
  test: the three legs are each covered (the shell fires the event, the
  bridge re-reads and sets the factory, the tile hands the delegate its
  colours on a delivered snapshot) but nothing joins them, which would
  need a real service and a real requery.
- `examples/demo-config/` has no `colours.toml`, so `--demo` shows
  nothing coloured.

### 9.7 Task 7 — the theme checks, and the readability floor

The check as designed found the unfloored wheel unreadable on **27 of 44
bundled entries — 244 of 1,056 (theme × hue × tone) checks, 23%**, worst
1.22:1 against a 3:1 floor, concentrated in Light-tone hues on light
backgrounds plus a handful of dark themes whose anchors sit close in
luminance to their own background. Two rounds of ruling followed (both
recorded below): a named 107-pair exception list, then — the user's
ruling — a **readability floor in the resolver itself**, which deleted
the exception list and the per-tone split outright.
`every_bundled_theme_keeps_generated_hues_readable` now asserts,
unconditionally and with no exceptions, that every one of the 12 hues at
30° steps in **both** tones clears `READABLE_RATIO` against the theme's
background, for every bundled entry; it reports the smallest OKLCH anchor
arc per theme and the per-theme floor count.

**The design finding, stated plainly: §2.2's light tone is not usable as
a text colour on light themes without the floor.** A theme's `.light`
anchors are tints — authored as fills, accents and chart swatches, not as
text on the theme's own background.

**Per-theme floor counts** (of 24 hue/tone pairs each) are the retune
work order. Everforest Light and Solarized Dark need the floor on **all
24** pairs, Catppuccin Latte 23, Ayu Light 22, Mellifluous Light 20 —
those five are what the committed test's own `eprintln!` prints.

| Theme | Pairs floored | | Theme | Pairs floored |
|---|---|---|---|---|
| Everforest Light | 24 | | Adventure Time | 6 |
| Solarized Dark | 24 | | macOS Classic Light | 6 |
| Catppuccin Latte | 23 | | Flexoki Dark | 5 |
| Ayu Light | 22 | | Everforest Dark | 3 |
| Mellifluous Light | 20 | | Kibble | 2 |
| Molokai Light | 16 | | Tokyo Night | 2 |
| Flexoki Light | 15 | | Twilight | 2 |
| Molokai Dark | 14 | | Adventure | 1 |
| Default Light | 13 | | Alduin | 1 |
| Aurora Light | 13 | | Ayu Dark | 1 |
| Gruvbox Light | 10 | | Gruvbox Dark | 1 |
| Fahrenheit | 9 | | Hybrid Light | 1 |
| Hybrid Dark | 8 | | macOS Classic Dark | 1 |
| | | | Tokyo Moon | 1 |

Floored on zero pairs — already fully readable without it (17 entries):
Asciinema, Bloomberg, Bloomberg Modern, Catppuccin Frappe, Catppuccin
Macchiato, Catppuccin Mocha, Default Dark, Harper, Jellybeans,
Mellifluous Dark, Modus Operandi, Modus Vivendi, Nord, Solarized Light,
Spaceduck, Tokyo Storm, TradingView Dark.

**The anchor-arc report** is the other half — a folded palette as a known
number rather than a surprise. The five smallest: **Hybrid Dark 0.000°**
(two of its six normal-tone anchors resolve to numerically the same OKLCH
hue angle — a second, independent finding about that theme's anchor set),
Spaceduck 1.231°, Alduin 2.287°, Tokyo Night 10.171°, Ayu Dark 12.067°.
The widest is Bloomberg at 50.043°.

**`readable_on`'s fallback, which the code does not guard and must not be
"fixed" blind.** The bisection assumes moving toward `toward` crosses the
ratio somewhere in `t ∈ [0, 1]`. When it does not — foreground and
background on the same side of the colour's lightness, or equal — the
loop's `hi = 1.0` endpoint is returned **untested**: maximally moved,
still unreadable. No bundled theme reaches it (the test asserts every one
of those 1,056 pairs past the floor), so it is latent for a user-authored
theme alone. Its doc comment names it.

**Harness, at §7.** The design's own "generated hues must clear 3:1"
entry was written and **survived by construction** in its first form,
because the assertion was already failing on real unmutated data — a
weakening to `|| true` just turned the test green. It was replaced by
`colour: contrast_ratio applies the WCAG +0.05 floor` (mutating the
formula the check reads, under `geode-core`'s own test), and then, once
the floor made the theme test pass through the real mechanism, joined by
two entries on `readable_on`'s early return: `colour: the readability
floor pulls lightness until 3:1` (unit level) and `theme: bundled themes
clear 3:1 through the resolver` (bundled-theme level). The two share a
file and anchor deliberately — `readable_on` has exactly one early
return, so neither is ambiguous, and each `run_mutation` mutates, tests
and restores before the next runs.

### 9.8 Rulings

- **A readability floor in the resolver** (user ruling, 2026-09-13, Task
  7 fix round 2). `readable_on(rgb, background, toward)` pulls OKLCH
  lightness until 3:1, hue and chroma kept; applied to `Definition::Hue`
  only, never to a token; `Tokens` gains a `background` field that is not
  a nameable `Token`; the theme test asserts both tones everywhere with
  no exceptions and reports floor counts as the retune work order.
  §2.2's identity rule gains "unless unreadable" (amended above).
  **Cost:** an anchor hue on an unreadable theme is no longer *exactly*
  that theme's colour — its lightness has moved. This supersedes the
  previous round's ruling (normal tone asserted with a named
  `KNOWN_LOW_CONTRAST` list of 107 pairs across 21 themes, light tone
  report-only with the reason recorded), whose list and per-tone split
  were deleted with it.
- **A cleared label or an `auto` width means "as the desk has it"**
  (ruling on Task 4's review). The fold resolves to the desk's own value
  (`None` where the desk sets none), the writer drops the now-equal key,
  the field re-seeds so the desk's value reappears on the same keystroke,
  and the notice reads `"<key> follows the desk again"`. **Cost:** a
  trader cannot delete a desk-declared label from the dialog, only stop
  overriding it — which is the overlay's semantics. §4.3's own
  impossibility stands beside it: a key cannot be *pinned* at the value
  the desk currently has, because an equal key is never written (the
  review's Minor 7, taken as a rule rather than fixed).
- **The writer compares through `kind_default`, not raw `Option`s** (Task
  4, judged correct against §4.3's own definition of the baseline). The
  five `ColumnFormat` keys compare `effective.<key> !=
  desk_format.<key>` — both sides resolved through the kind default —
  while `label` and `width` compare as bare `Option`s, having no kind
  default to resolve against. Without it the column stage froze the kind
  defaults into every trader's overlay: `fold_into` writes a `Some` for
  all five format keys (its own test requires it), so a raw-`Option`
  compare called all five "different from the desk" and one step of
  `Scale` wrote five keys.
- **The cache's shape is pure** (Task 6 preflight ruling):
  `ColourCache::get(colours, name, anchors, tokens)`, keyed on
  `(Anchors, Tokens)` and never taking a `Theme`; the definitions' own
  invalidation is `Arc::ptr_eq` in the delegate's `set_colours`.
- **Task 3's fix round takes Importants 1–2 plus Minors 1–2** (the
  wrong-reason harness entry, the untested
  `kind_default`/`column_summary` pair, and two doc sentences); the other
  Minors deferred, costing a slightly wider fix diff.

### 9.9 Deferred — review minors taken and recorded rather than fixed

**Task 1.** `interpolate_hue`'s `t` is `(h − anchor)/60` rather than the
spec's `(h mod 60)/60` — a direct caller passing exactly 360 after
`rem_euclid`'s boundary quirk would extrapolate (unreachable from
config). `Token::Chart(_) => "chart.5"`'s catch-all disagrees with
`Tokens::get`'s clamp for `Chart(0)`/`Chart(6)` (unreachable via
`parse`). One message covers reserved-or-invalid names, and
`check_object_name`'s trimmed return is discarded (a quoted `" delta "`
key is stored untrimmed). `Definition::summary` truncates a fractional
hue (`210.5` → `"hue 210"`), which Task 5's browse row shows.
`load_views`' cross-check clones each `ColumnPresentation` via
`presentation_of`, and a `Named` colour in the presentation map for a
column the view lacks is not cross-checked (it is unpaintable anyway).
The Task 1 report claims a comment on `ColumnFormat::with` that is not
there.

**Task 2.** The `RefCell<Vec<Diagnostic>>` + `&dyn Fn` warn pattern would
be simpler as `&mut dyn FnMut` (the brief pinned `&dyn Fn`). An
unrecognised key inside a `[view.columns.<col>]` table is silently
dropped — consistent with the desk's own `format` table.

**Task 3.** `render.rs`'s item rows call `views::kind_default` /
`column_summary` for every domain (Groupings yields `""` only because it
seeds a default presentation). The summary renders a fractional width
rounded (`120.5` → `"120 px"`) while the file holds `120.5`. Only 7 of
the ~100 harness entries anchored in `mod.rs`/`render.rs`/`views.rs` were
re-run after the `width` → `presentation` rename; Task 8's full
`--changed` run covers them.

**Task 4.** `desk_baseline` walks the source's columns on every
column-stage keystroke (modal, bounded by typing speed). A member-row
**click** still only selects — no mouse parity with `enter` for the
column stage. The value field's label in the column stage reads `tree ·
Width` (the object) rather than `npv · Width`.

**Task 5.** The swatch harness entry is covered by the whole window test
rather than a unit test. Two files are named `colours.rs`, disambiguated
by path and alias. `mod.rs` is 5,359 lines and `render.rs` 3,898 after
this task — a file-size trend worth watching.

**Task 6.** `Anchors`/`Tokens` are derived per visible **cell** of a
named column (N × 28 `Hsla → Rgb` conversions plus N key compares per
frame) rather than once per column, where §6.3 says one comparison per
frame; the fix is to stash the derived pair on the delegate and compare
two sentinel `Hsla`s before re-deriving. **Marked likely-to-fix before
merge.** `render_th`'s colour branch has no test and no harness entry.
`cell_colour`'s doc over-claims "painted in foreground" for the header
caller, which inherits the header colour. `ColourCache::get` duplicates
`resolve_named`'s resolution. `BlotterFactory::colours()` has only a test
caller. Every `ConfigReloaded` mints a fresh `Arc<NamedColours>` and so
invalidates every tile's colour cache, where `content.rs`'s comment
claims a tighter invariant.

**Task 7.** `background: grey(0.1)` in `geode-core`'s own `tokens()`
fixture is load-bearing for
`resolve_uses_the_token_field_and_the_tone_anchors`' exact equality and
says nothing about why. The floor report counts *whether* a pair was
floored, not how far — a 1% nudge and a near-foreground drag read the
same in the work order. The two `readable_on` tests bound chroma only
from above (`got.c <= orig.c + 1e-4`), so a collapse to grey would pass.
And the floor measures against `theme.background` alone: a cell over a
row stripe or a selection highlight can still read below 3:1.

### 9.10 Harness

**688 entries** on the branch (656 at the branch point, 32 added over the eight tasks,
none removed; 741 once main's market-data documents Part 1 was merged in); `--anchors-only` reports 0 stale, 0 ambiguous. Ten
entries were re-anchored where these tasks moved their source lines —
eight predating this branch, two added earlier on it: `views: a
presentation save pins the desk's column order`, `views: a presentation
save copies the
desk's widths into the user's file` (also re-pointed at a covering test
that can actually see it, then re-mutated to `if true` — §9.3),
`objectdialog: Number refuses to step down` (found **stale** by
`--anchors-only`, predating this branch), `views: the overlay writer
omits keys equal to the desk`, `objectdialog: set_query mirrors into the
open draft only in the edit stage`, `objectdialog: effective_query reads
the edit stage's draft`, `objectdialog: a presentation field is written
to the view's own doc` (the bare `dest: Destination::Presentation,` line
now matches three times, so the anchor carries the `kind:` line above
it), `objectdialog: a reverted write leaves the column stage`, `reload: a
view_presentation change triggers the same reload a views change does`,
and `bridge: reload reports presentation diagnostics`.

One pre-existing entry still prints a `FILTER` warning and falls back to
its crate suite, where it is caught: `groupings: fields duplicates a
chain column instead of deduping against the catalogue` names a test
renamed to `…_groupable_…` before this branch. It is on `main`; this
branch did not touch it.

### 9.11 Display checks pending

No sandbox in this branch painted a window, as on every Phase 4c branch.
Unverified pixel-for-pixel: the browse-row and edit-header **swatches**;
the blotter's **header colour** through the overridden `render_th`; the
member row's **presentation summary** (`npv · 120 px · k · 0 dp ·
delta`); the column stage's **crumb** (`tree › npv`), its seven rows and
its own footer hint line; and that stage's value-field label (which reads
`tree · Width`, the deferred minor above). Everything else in this
section is verified against window-test assertions, unit tests, the
harness and the code directly.

### 9.12 Final review

The whole-branch review of `2a28214..9cd103b` (19 commits including the
merge of main, 33 files, +7,455 / −367) found **no Criticals, two
Importants (I-1, I-2) and eight Minors (M-1..M-8)**, and confirmed the
branch correct on every property it could reach: the overlay writer
cannot fork a desk view or freeze a desk key, the column stage's
projection cannot leak into the view's own write, the colour cache
cannot paint under a stale theme or stale definitions, and the merge of
main is clean (harness arithmetic exact at 709 + 688 − 656 = 741, no
duplicate names, every anchor unique). Verdict: **ready to merge with
fixes**. One fix wave followed, carrying **I-1, I-2, M-1, M-2, M-3, M-4,
M-6 and M-8 as code and M-5, M-7 as doc sentences** — the whole set.

**Rulings**

- **I-1 is fixed with the 28-value `[Hsla; 28]` theme signature, not the
  two-sentinel sketch §9.9 recorded.** Two sentinels (`background` +
  `foreground`) miss an anchor move that leaves those two equal, and the
  stale derived pair *is* the `ColourCache`'s own key, so it could never
  notice: the memo would keep painting the old colour with nothing in
  the system able to see it. The full signature is exact and barely
  larger — 28 field reads and no arithmetic — so the steady path is 28
  `Hsla` copies plus 28 `Hsla` compares and **zero** conversions, which
  is the "one comparison a frame" §6.3 asked for.
- **I-2: `d` and `r` are refused in the column stage, through the same
  `not_a_column_verb` notice `x`/`shift+j`/`shift+k` already use.** The
  crumb has narrowed the object to one column, and a destructive verb
  must not answer about the whole view — `d` deletes the user-layer view
  outright, `r` reverts the trader's personalisation of every column of
  it. Leaving two destructive verbs live where three navigational ones
  were explicitly refused is the asymmetry that reads as an oversight.
  **Cost: a trader presses `escape` first.**
- **The one fix wave carries I-1, I-2, M-1, M-2, M-3, M-4, M-6 and M-8
  as code and M-5, M-7 as doc sentences.**

**What each fix changed**

- **I-1** — `shell::colours::theme_signature(&Theme) -> [Hsla; 28]` (12
  anchors then 16 token colours, in each derivation's own field order,
  with a doc saying a colour added to either must be added here too).
  `BlotterDelegate` gained `theme_inputs: Option<([Hsla; 28], Anchors,
  Tokens)>` and `ensure_theme_inputs`, and both paint sites now go
  through one door, `themed_cell_colour(col_ix, theme)`. The read stays
  lazy — the memo is `None` until a named cell paints, so a blotter
  naming no colour still builds nothing. `ColourCache`, its key,
  `set_colours` and `invalidate` are untouched. The column-name lookup
  moved into a free `named_colour_of(plan, col_ix)` so both methods can
  hold it while the cache takes its `&mut`.
- **I-2** — one `in_column_stage` guard at the top of `arm_delete` and of
  `arm_revert` (`objectdialog/render.rs`). Guarded *inside* the two
  functions rather than at the dispatch arm where `x`'s guard sits,
  because each has two callers — the keystroke and `press_verb`'s
  action-bar click (§18.9 made the bar the mouse form of these letters)
  — and a keyboard-only guard would leave the stage destructible with a
  mouse, the Part 2b review Major again.
- **M-1** — `Draft::enter_column` returns `false` when `self.column.
  is_some()`, making re-entry unrepresentable rather than merely
  unreached: the membership test passes *through* `field_by_key`'s
  parent fallback, so a second entry would stash the seven installed
  column fields as `parent_fields` and drop the view's own list forever.
- **M-2** — `colours::to_table` keeps the `hue` item it removed and
  writes it back verbatim whenever the field's value still equals
  `source["hue"].round()`, so a hand-edited `hue = 210.5` survives a step
  of `tone` or `token`. Stepping the hue itself still writes the field.
- **M-3** — `view.rs`'s legacy `width` fold validates before inserting:
  the conflict check became a lookup and the entry is created only on a
  positive number, so one invalid width is one diagnostic instead of two
  (its own, plus `apply`'s spurious "the view does not have that
  column").
- **M-4** — `ViewPresentation` gained `legacy_keys: BTreeMap<String,
  &'static str>`, recording the spelling each entry was created under
  when it was not `columns`, and `apply`'s column-not-in-view warning now
  names that spelling (`'hidden' names column 'ghost'`, path
  `view_presentation.<view>.hidden`; `'width'` with path
  `…width.<col>`; `'columns'` by omission).
- **M-6** — `enter_column_stage` reads the `colours` doc through
  `apply::config_with_pending`, so the two doors into a stage agree about
  what "the live config" means inside the 250 ms write debounce.
- **M-8** — the browse list hoists `anchors_from_theme`/
  `tokens_from_theme` out of the row loop beside the `NamedColours` it
  already hoisted, and resolves each row through
  `geode_core::colour::resolve`. `colours::resolve_named` is now the
  single-colour door and its doc says so.
- **M-5** — `Draft::fold_column`'s doc names its coupling: it calls
  `views::desk_baseline`/`views::fold_into` by name where `enter_column`
  deliberately takes the fields in, and a second domain gaining a column
  stage must be handed the fold rather than inheriting Views'
  desk-fallback semantics.
- **M-7** — `views::presentation_table`'s doc states that this writer
  fully owns the overlay's `[view.columns.*]` block: built from `items`
  rather than from `draft.source`, so a hand-written key the vocabulary
  does not model is dropped on the next save — matching the reader, and
  the pre-2c behaviour of `order`/`hidden`/`width`.

**Deferred minors closed.** §9.9's Task 6 entry — "`Anchors`/`Tokens` are
derived per visible **cell** … marked likely-to-fix before merge" — is
closed by I-1, and its sketched two-sentinel fix is superseded by the
ruling above. Nothing else in §9.9 is closed by this wave; the Task 1
`Definition::summary` fractional-hue truncation stays open as a
*display* (M-2 fixed only the *write*).

**Tests and harness.** Four new tests, one per code finding that has an
observable behaviour: `the_theme_input_memo_re_derives_only_when_a_
theme_colour_moves` (blotter, asserted through a deliberately poisoned
memo, since a re-derivation under an unchanged theme produces an equal
pair and an assertion on the value would pass either way),
`delete_and_revert_are_refused_in_the_column_stage` (window test; the
fixture flushes a real presentation override first, so `r` would arm
absent the guard), `enter_column_refuses_re_entry_and_keeps_the_objects_
own_list`, `a_fractional_hue_survives_a_save_that_did_not_step_it`,
plus `an_invalid_legacy_width_on_an_unknown_column_is_one_diagnostic`
(M-3) and `the_column_not_in_view_warning_names_the_spelling_it_was_
read_under` (M-4). Four harness entries were added (one each for I-1,
I-2, M-1, M-2) and two re-anchored where this wave moved their lines —
`overlay: the column table wins over a legacy key` (M-3) and `colours:
to_table omits the hue under a token` (M-2); every one reports `caught`.
Two source lines were deliberately spelled apart from their twins to
keep an existing anchor unique: `enter_column_stage`'s `pending` local
against `enter_edit_stage`'s `folded`, and `themed_cell_colour`'s cache
call against `cell_colour`'s. **745 entries**, `--anchors-only`: 0 stale,
0 ambiguous.

**Display checks.** §9.11's list is unchanged by this wave — none of
these fixes moves a pixel except I-1, which paints the same colour by a
cheaper route.
