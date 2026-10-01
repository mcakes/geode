# geode-nemo

Row menu actions that hand a position or an instrument to Nemo, the desk's
pricing app, by URL. The crate provides two "Open in Nemo" actions: one on the
`position_ref` column, opening `nemo://position/{id}`, and one on the
`instrument_ref` column, opening `nemo://instrument/{id}`. Both URL shapes are
provisional until Nemo's own scheme is known. The id is percent-encoded (RFC
3986 unreserved characters pass) so any id stays one path segment. An action
uses the target row's value only; a selection is ignored. After opening the
URL it posts the notice `opened <url>`, which says what Geode did and never
that Nemo launched: the OS reports nothing back. `actions()` returns both,
position first, for `geode-app` to register on the module roster; the shell's
row menu runs them.

## Commands

```sh
cargo test -p geode-nemo
```
