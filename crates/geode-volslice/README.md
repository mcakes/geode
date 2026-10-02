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
  pair of loaded kinds).

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
- Following is compared through `FrameView::following()` on every frame
  notification; while following, the group's scope counts as a change.
- An arrival made from `set_visible` is deferred past the current draw,
  where a notify to the frame would be dropped.
- The picker holds the keys in `insert` mode and publishes no `tilelist`:
  its field types `j` and `k`, which the shared list steps would claim; the
  arrows step it. The diff chooser has no field: it reports `mode == menu`
  and publishes `tilelist`, so the shell's `j`/`k` step it and the strip's
  own `j`/`k` stay out.
- `space` refuses to deactivate the last active expiry; `enter` on another
  row moves off it.
- While following, the underlying is the group's: `u` and `:underlying` are
  refused, and a launched follower is not prompted.
