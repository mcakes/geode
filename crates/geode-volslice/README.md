# geode-volslice

The vol slice viewer tile: one underlying's volatility smiles, one curve per
expiry and per kind, read from the CVI document, a link group's draft and the
option chain. Every vol, coordinate and density it paints comes out of the
data tier's vol door; the crate computes none of them.

## Module map

- `content.rs`: `VolsliceFactory` (kind `volslice`, accepts `underlying_ref`,
  launch table `{ underlying = "<u>" }`), the tile's `TileContent` door
  (`follows()` is `true`), `ACTIONS` and the `DEFAULT_KEYMAP` fragment.
- `commands.rs`: the `:` line (`underlying <ref>`, `x <coordinate>`,
  `diff <kind> - <kind> | off`, or with the Unicode minus) and its
  completions.
- `core/`: pure state, tested without gpui. `build.rs` turns a batch's
  answers into the xy model (a failed job is a notice, a short outcome
  builds nothing); `session.rs` round-trips the session table, dropping an
  unreadable key with a notice and clamping the split to the chart's bounds.
  `model.rs`'s `toggle_pair` is the one door that turns a difference on or
  off, keeping turn-on order and never a pair beside its reverse.
- `tile/`: the hosted entity and its tests. `tile/data.rs` is the data
  flow: the two document reads, the followed group's board, the vol batch
  and the model swap. `tile/picker.rs` holds the two choosers: the
  underlying picker (a field over the diagnostics catalog's `cvi_params` and
  `option_chain` underlyings) and the diff chooser (every ordered pair of
  loaded kinds, ticked). `tile/pointer.rs` holds the chart's wheel, drag
  and divider gestures and the strip's presses.
- `header.rs`: the header (underlying, coordinate, a chip per loaded kind
  with its digit, the diff chip, link chips, the two datasets' health, the
  shell's × last) and the footer (the first notice with a count of the rest,
  the key hints).
- `strip.rs`: the expiry strip beside the chart: a dot in the expiry's
  color (filled when active), the date and a digit per kind that has the
  expiry.

## Commands

```sh
cargo test -p geode-volslice
cargo bench -p geode-volslice
```

The `model_build` bench times `core::build::model` alone over twelve active
expiries, the three kinds, densities and two differences (`cvi draft −
chain` and `cvi − cvi draft`), with the batch answered once outside the
loop. Its target is under 1 ms; it measured about 200 µs with 1,000-point
curves (see the performance guide).

## What it paints

The kinds are fixed, in header order: `cvi` (the published CVI document as
of the frame, digit `1`), `cvi draft` (the followed group's board draft,
`2`) and `chain` (the option chain, `3`). A curve kind is one solid
(published) or dashed (draft) line per active expiry, evaluated at
`GRID_N` (1,000) strikes on the document's strike range widened to the
chain's lowest and highest listed strike wherever a chain is loaded at that
expiry (`Grid::Dense { cover }`, from `core::build::cover`). The cover is
asked whether or not the chain is shown, so toggling the chain does not move
a curve's x extent; an expiry with no chain covers nothing. The chain is its
mid vols as points with the bid-ask range as a bar, a one-sided quote as a
half bar.

Each expiry takes the `HuePalette` color of its strip position: the theme's
first chart color's hue plus one golden angle (about 137.5°) per position,
so neighbouring expiries are far apart in hue and no hue repeats. The
published curve and the draft paint in that color (the draft dashed); the
chain paints in the position's companion, a lighter-weight shade of the
same hue, moved the same way for every expiry of a theme (paler where all
the first 24 can stay readable that way, otherwise toward the foreground).
`core::build::model` resolves each active expiry's color and companion
once per build.
`shift+d`
adds each visible curve's density on the right axis at `DENSITY_ALPHA` of
the expiry color, shaded down to zero (a filled xy line, so a negative lobe
shades up to zero), per unit of the shown coordinate (a gap where delta
saturates, in line and shading alike). The model spaces a dense grid; the
demo model packs it toward the forward on the scale of σ√t, so a
short-dated density keeps 25 or more points per σ√t·F however wide the
chain and reads as a smooth hump.

Difference pairs (`d`) paint in a lower pane under the vol pane, several at
once, expiry by expiry in the order they were turned on. A pair and its
reverse are never both on.

- Curve minus curve is at equal strike: the minuend is evaluated dense and
  the subtrahend at the minuend's strikes (`Grid::Job`), plotted at the
  minuend's x as a line.
- Curve minus chain is at the chain's strikes: the curve is evaluated `At`
  them and the difference sits at the chain's x as points with whiskers:
  `curve − chain` is `curve − mid` from `curve − ask` to `curve − bid`,
  `chain − curve` is `mid − curve` from `bid − curve` to `ask − curve`; a
  one-sided quote leaves a half bar.
- A job two pairs share (a dense curve, the chain's map, a curve at the
  chain's strikes) is asked once per expiry; `Role::DiffCurve { at }` names
  whose strikes an evaluation is at.
- Each difference keeps its expiry's hue (`core::build::diff_color`). Two
  pairs at one expiry differ by mark (line or points with whiskers), and
  the two pairs with the chain by color: `cvi`'s in the expiry's color, the
  draft's in its companion.
- A hidden kind's own trace is not asked, but a pair that names it still
  gets its jobs, so hiding a kind to read the difference works.

The strip is the sorted union of every loaded kind's expiries, with none
before today, each row marked with the digits of the kinds that have it.
A curve paints at every strip expiry, not only at its document's terms: the
vol model interpolates between terms and extrapolates past them.
The first strip fronts its first row; a restored set that names no listed
expiry fronts the first row too, and an empty strip keeps the set for when
data returns.

## Keys

| Key | Action |
|---|---|
| `j` / `k`, `down` / `up` | Strip cursor |
| `space`, `enter` | Show only the cursor's expiry |
| `ctrl+space`, `shift+space` | Add the cursor's expiry, or take it out (the last active one stays) |
| `1`..`3` | Show or hide a kind (`4`..`9` are bound and do nothing) |
| `x` | Cycle the coordinate: moneyness, log-moneyness, delta, strike |
| `shift+d` | Densities on or off (beats the workspace's duplicate in the tile) |
| `d` | Difference chooser: every ordered pair of loaded kinds, ticked |
| `u` | Underlying picker (refused while following) |
| `h` / `l` | Pan by a tenth of the view, the way the axis reads |
| `=` / `-` | Zoom about the view's centre |
| `0` | Reset the view to the data's extent |
| `[` / `]` | Shrink or grow the upper pane by a twentieth, within 0.2..0.8 |

In the picker, `enter` commits, `escape` cancels, `up`/`down` step and every
other bare key types; `tab` completes. In the chooser the shared `j`/`k` or
arrows step, `space` ticks or unticks the highlighted pair (unticking its
reverse), `ctrl+x` unticks every pair (a touch, so `ctrl+x enter` shows
none), `enter` applies the ticks and `escape` discards them; it opens
with the shown pairs ticked, and an untouched, empty tick set applies the
highlighted pair alone, as the shell's dimension picker does. A row click
is `space` and the Apply row `enter`. A strip click solos its row and a
ctrl+click, shift+click or right press adds or removes it, on a focused
tile (macOS delivers ctrl+click as a right press with control cleared; the
tile answers no `press_context`, so the shell's right-press route only
focuses; a left press holding alt or cmd is the shell's); a kind chip click is its digit and the
diff chip `d`; the wheel zooms about the pointer or pans, a plot drag pans
and a divider drag sets the split.

## Commands and session

The `:` line takes `underlying <ref>` (refused while following), `x
<strike|moneyness|log-moneyness|delta>` and `diff <kind> - <kind> | off`,
where the minus is ` - ` or `−`: a pair toggles that difference (turning it
on turns its reverse off) and `off` turns every pair off; `none` is read as
`off`. Turning on a pair naming a kind that is not loaded is refused
`<kind> is not loaded`; turning one off never is.

The session table holds `version = 1`, `coordinate`, `hidden`, `density`
and `split` always, and `underlying`, `expiries`, `diffs` (a list of
`[minuend, subtrahend]` pairs in turn-on order) and `view` while set; the
cursor is not saved. A session holding the single-pair `diff` key of an
older build restores it as one pair; `diffs` wins beside it. A value that
cannot be read drops its key with `session: dropped <key>: <why>`, and so
does a `diffs` list naming a pair twice or a pair and its reverse; a split
outside the chart's bounds is clamped with a notice. The launch table
`{ underlying = "<u>" }` restores through the same reader.

## Failure states

The footer shows the first notice and how many more stand behind it. An
empty state alone (the first item below) is painted in the muted status
tone the sibling modules use for an empty state; every other notice is a
failure or a refusal, painted in the danger tone, and so is an empty state
with another notice behind it:

- `no underlying`, or `no underlying in A` while following a group whose
  scope names none or several.
- `document request refused: …` when a read is refused: a refused CVI read
  asks again on the next change; a refused chain read fails the fetch,
  which still answers the flip barrier. The last good documents stay when
  they are the asked underlying's; another underlying's documents, strip
  and curves clear, so none of them sits under the new name.
- `no CVI document for <u>`, `no option chain for <u>`, or the reader's
  error for a snapshot it cannot read (`option_chain rows for <date> are
  not contiguous`); the other kind still paints.
- `no <kind> curve at <date>: <why>` and `no chain coordinates at <date>:
  <why>` for a failed job, once each; the rest of the batch paints. A
  difference that fails only because the curve it reads its strikes from
  failed adds no notice of its own. A chain whose coordinates do not count
  one per quote is skipped with `no chain coordinates at <date>: <n>
  coordinates for <m> quotes`. One cause behind every job is said once, in
  the outcome's words (`vol model "demo" is not built into this binary`,
  `the vol queue is full; resubmit`).
- `diff <pair>: <kind> is not loaded`, once per pair, when a restored or
  kept pair names a kind with nothing loaded (a draft that left with its
  group): that pair asks no difference, the others paint, and the pair
  stays on for when the kind returns.
- `vol request refused: …`: the painted model stays only when it was
  built from exactly the documents on screen (same underlying, CVI
  document, chain and draft with its mark), and the next change retries.
  Curves built from anything else (another underlying's documents, a
  draft that since left or changed mark, a superseded publication) clear
  under the new strip and chips.
- `following A — set the underlying there` for `u` while following.

## Known limitations

- Expiries before today are dropped by the viewer, not the data tier: the
  dataset headline can still pin to an expired document.
- One chain kind: source identity is absent from the chain's rows and
  provenance, so two chain sources cannot be told apart.
- The flip barrier covers the documents only. The vol batch is a follow-on,
  so the painted curves swap one vol round trip after the flip releases;
  for that round trip the header, chips and strip already name the new
  underlying over the old curves.
- Expiry colors follow the strip position, so a draft that adds a term
  shifts the colors of the rows after it. Positions 21 rows apart sit about
  8° apart in hue (13 apart, 12°) and can look alike when both are shown.
- On macOS `ctrl+space` may be taken by the system's input-source shortcut;
  `shift+space` is the same verb.
- Keyboard zoom anchors at the view's centre; only the wheel anchors at the
  pointer.
- There is no `.` action menu: the header chips are clickable and the
  palette lists every action.
- A standing `vol request refused` notice is retried by the next state
  change (a key, a draft edit, a publication), not by a group scope change
  that keeps the underlying.

## Invariants

- The key context does not opt into counts: the bare digits are kind
  toggles, and a counting context would swallow them.
- The CVI and chain reads run in sequence under one `FollowingQuery` tag,
  both keyed by the tile: the query pool keeps one request per key and the
  flip barrier one entry per key, so concurrent reads would supersede each
  other. The pair is one barrier arrival; a refused chain read fails the
  fetch, which arrives too.
- The vol batch and the board are never staged behind a flip. A board
  draft reaches the batch only while it names the underlying whose
  documents are loaded.
- The header names `loaded_for`, the underlying whose documents are on
  screen, and the asked underlying only while nothing is: a document read
  in flight or a failed one never puts a new name over the old picture.
  The one exception is the vol round trip after new documents install,
  when the new name and strip stand over the old curves until the batch
  lands (see Known limitations).
- `loaded_gen` moves on every change to the loaded documents and the model
  records the generation it was built under, so a refused batch clears
  curves that no longer match what is loaded, even under one underlying.
- Following is compared through `FrameView::following()` on every frame
  notification; while following, the group's scope counts as a change,
  except one that still names the loaded underlying with no read out,
  nothing staged, and the as-of and publications unmoved since the loaded
  documents were read (both reads succeeded under them, `loaded_ok`;
  `documents_stale`): the tile self-arrives instead of refetching, on a
  show through the deferred door. A failed or refused read clears
  `loaded_ok`, so the next scope change retries it. A
  change of group clears the model and moves the vol tag, so no old
  group's draft trace stays painted or lands late.
- An arrival made from `set_visible` or `closed` goes through
  `geode_tile::following::DeferredDoor` (`Arrival::Deferred` for the
  requery): the shell calls both inside its render, where the release's
  notify to the frame would be dropped and every other tile would wait for
  the barrier's deadline.
- The picker holds the keys in `insert` mode and publishes no `tilelist`:
  its field types `j` and `k`, which the shared list steps would claim; the
  arrows step it. The diff chooser has no field: it reports `mode == menu`
  and publishes `tilelist`, so the shell's `j`/`k` step it and the strip's
  own `j`/`k` stay out.
- `ctrl+space` and `shift+space` refuse to deactivate the last active
  expiry; `space` or `enter` on another row moves off it.
- `space` belongs to the strip in `normal` mode and to the chooser's tick
  in `menu` mode: the chooser's binding is scoped by mode, so a tick never
  solos an expiry.
- `State::diffs` never holds a pair beside its reverse or twice: the
  chooser, `:diff` and the session reader all go through `toggle_pair` or
  refuse such a list.
- While following, the underlying is the group's: `u` and `:underlying` are
  refused, and a launched follower is not prompted.
- Paint formats nothing. The header text, the strip rows and the footer
  notice are rebuilt when the tile is notified and an input they were
  built from changed, and directly from `set_visible`, which the shell
  calls inside its draw where the notify is dropped; the hints when the
  chords change. `set_focused` sends no notify for the same reason: the
  flag is painted in that frame because the tile's view is an uncached
  child of the shell's.
- Every pointer action has its key: a strip press is `space` (ctrl or
  shift: `ctrl+space`/`shift+space`), a kind chip its digit, the diff chip
  `d`, a chooser row `space` and its Apply row `enter`, a strip right press
  `ctrl+space`, the wheel `=`/`-`
  and `h`/`l`, a drag `h`/`l`, the divider `[`/`]`. A strip press acts only on a tile the
  shell had already told it is focused (`TileContent::set_focused`), so the
  press that focuses a tile changes nothing else.
- Pans and zooms go through the x axis's scale (`pan_sign`, `about`), so a
  reversed delta axis moves the way it reads. Keyboard zoom anchors at the
  view's centre, wheel zoom at the pointer.
