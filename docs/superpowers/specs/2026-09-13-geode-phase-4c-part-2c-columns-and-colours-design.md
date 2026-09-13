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
