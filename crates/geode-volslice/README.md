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
  `diff <kind> - <kind> | none`, or with the Unicode minus) and its
  completions.
- `core/`: pure state, tested without gpui. `build.rs` turns a batch's
  answers into the xy model (a failed job is a notice, a short outcome
  builds nothing); `session.rs` round-trips the session table, dropping an
  unreadable key with a notice and clamping the split to the chart's bounds.
- `tile/`: the hosted entity and its tests. `tile/data.rs` is the data
  flow: the two document reads, the followed group's board, the vol batch
  and the model swap. `tile/picker.rs` holds the two choosers: the
  underlying picker (a field over the diagnostics catalog's `cvi_params` and
  `option_chain` underlyings) and the diff chooser (`none` and every ordered
  pair of loaded kinds). `tile/pointer.rs` holds the chart's wheel, drag
  and divider gestures and the strip's presses.
- `header.rs`: the header (underlying, coordinate, a chip per loaded kind
  with its digit, the diff chip, link chips, the two datasets' health) and
  the footer (the first notice with a count of the rest, the key hints).
- `strip.rs`: the expiry strip beside the chart: a dot in the expiry's
  palette color (filled when active), the date and a digit per kind that
  has the expiry.

## Commands

```sh
cargo test -p geode-volslice
cargo bench -p geode-volslice
```

The `model_build` bench times `core::build::model` alone over twelve active
expiries, the three kinds, densities and a `cvi draft − chain` difference,
with the batch answered once outside the loop. Its target is under 1 ms; it
has not been measured locally (see the performance guide).

## What it paints

The kinds are fixed, in header order: `cvi` (the published CVI document as
of the frame, digit `1`), `cvi draft` (the followed group's board draft,
`2`) and `chain` (the option chain, `3`). A curve kind is one solid
(published) or dashed (draft) line per active expiry, evaluated `Dense(200)`
on the document's strike range; the chain is its mid vols as points with the
bid-ask range as a bar, a one-sided quote as a half bar. Each active expiry
takes the chart palette color of its strip position, so twelve expiries
cycle the theme's five chart colors. `shift+d` adds each visible curve's
density on the right axis at a fixed lower opacity, per unit of the shown
coordinate (a gap where delta saturates).

A difference pair (`d`) paints in a lower pane under the vol pane:

- Curve minus curve is at equal strike: the minuend is evaluated dense and
  the subtrahend at the minuend's strikes (`Grid::Job`), plotted at the
  minuend's x as a line.
- Curve minus chain is at the chain's strikes: the curve is evaluated `At`
  them and the difference sits at the chain's x as points, negated when the
  chain is the minuend.
- A hidden kind's own trace is not asked, but a pair that names it still
  gets its jobs, so hiding a kind to read the difference works.

The strip is the sorted union of every loaded kind's expiries, with none
before today, each row marked with the digits of the kinds that have it.
The first strip fronts its first row; a restored set that names no listed
expiry fronts the first row too, and an empty strip keeps the set for when
data returns.

## Keys

| Key | Action |
|---|---|
| `j` / `k`, `down` / `up` | Strip cursor |
| `enter` | Show only the cursor's expiry |
| `space` | Show or hide the cursor's expiry (the last active one stays) |
| `1`..`3` | Show or hide a kind (`4`..`9` are bound and do nothing) |
| `x` | Cycle the coordinate: moneyness, log-moneyness, delta, strike |
| `shift+d` | Densities on or off (beats the workspace's duplicate in the tile) |
| `d` | Difference chooser: `none` and every ordered pair of loaded kinds |
| `u` | Underlying picker (refused while following) |
| `h` / `l` | Pan by a tenth of the view, the way the axis reads |
| `=` / `-` | Zoom about the view's centre |
| `0` | Reset the view to the data's extent |
| `[` / `]` | Shrink or grow the upper pane by a twentieth, within 0.2..0.8 |

In the picker, `enter` commits, `escape` cancels, `up`/`down` step and every
other bare key types; `tab` completes. In the chooser, `enter` commits,
`escape` cancels and the shared `j`/`k` or arrows step. A strip click solos
its row and a ctrl+click toggles it, on a focused tile; a kind chip click is
its digit and the diff chip `d`; the wheel zooms about the pointer or pans,
a plot drag pans and a divider drag sets the split.

## Commands and session

The `:` line takes `underlying <ref>` (refused while following), `x
<strike|moneyness|log-moneyness|delta>` and `diff <kind> - <kind> | none`,
where the minus is ` - ` or `−`; a pair naming a kind that is not loaded is
refused `<kind> is not loaded`.

The session table holds `version = 1`, `coordinate`, `hidden`, `density`
and `split` always, and `underlying`, `expiries`, `diff` and `view` while
set; the cursor is not saved. A value that cannot be read drops its key
with `session: dropped <key>: <why>`; a split outside the chart's bounds is
clamped with a notice. The launch table `{ underlying = "<u>" }` restores
through the same reader.

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
- `diff <pair>: <kind> is not loaded` when a restored or kept pair names a
  kind with nothing loaded (a draft that left with its group): no
  difference is asked, and the pair stays set for when the kind returns.
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
- Expiry colors cycle the five chart colors by strip position, so two
  expiries five rows apart share a color, and a draft that adds a term
  shifts the colors of the rows after it.
- Keyboard zoom anchors at the view's centre; only the wheel anchors at the
  pointer.
- There is no `.` action menu: the header chips are clickable and the
  palette lists every action.

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
- `space` refuses to deactivate the last active expiry; `enter` on another
  row moves off it.
- While following, the underlying is the group's: `u` and `:underlying` are
  refused, and a launched follower is not prompted.
- Paint formats nothing. The header text, the strip rows and the footer
  notice are rebuilt when the tile is notified and an input they were
  built from changed, and directly from `set_visible`, which the shell
  calls inside its draw where the notify is dropped; the hints when the
  chords change. `set_focused` sends no notify for the same reason: the
  flag is painted in that frame because the tile's view is an uncached
  child of the shell's.
- Every pointer action has its key: a strip press is `enter` (ctrl: `space`),
  a kind chip its digit, the diff chip `d`, the wheel `=`/`-` and `h`/`l`, a
  drag `h`/`l`, the divider `[`/`]`. A strip press acts only on a tile the
  shell had already told it is focused (`TileContent::set_focused`), so the
  press that focuses a tile changes nothing else.
- Pans and zooms go through the x axis's scale (`pan_sign`, `about`), so a
  reversed delta axis moves the way it reads. Keyboard zoom anchors at the
  view's centre, wheel zoom at the pointer.
