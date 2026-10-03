# geode-guide

The read-only `guide` tile displays `docs/user-guide.md` offline. `GuideFactory`
is the public seam; `geode-app` registers it independently of data setup,
after existing module kinds so the launcher's order stays stable.

| Module | Responsibility |
|---|---|
| `document` | Parse the bundled Markdown once, derive stable heading anchors and chapter entries, retain searchable sections, and render repository references as text. |
| `search` | Prepare rich-text blocks and visible text once; find literal case-insensitive matches and mark the same Unicode-safe ranges without altering links or formatting. |
| `tile::chrome` | Compose the shared header/actions menu, navigation, live shortcut hints, reader viewport, and search footer. |
| `tile` | Own the retained Markdown reader, section navigation, contents menu, keyboard scrolling, find preview, and frame-barrier acknowledgements. |
| `lib` | Module factory, tile contract, action registration, bindings, and opaque session state. |
| `tests` | Real-component keyboard, pointer, scrolling, find, and restoration checks. |

Open **Guide: Split** from the palette, or **guide** from the tile picker.
`c` opens chapter contents, `[`/`]` move through sections, `j`/`k` scroll,
Page Up/Down move a viewport page, and `g g`/`G` reach the section's ends.
`/` searches sections and `n`/`N` cycle matches. `y` copies section Markdown.
`:section <heading-anchor>` opens a heading directly with completion. The
header's **⋯** and `.` open the shared tile actions menu. Controls, tooltips,
the footer, and menu rows show live keymap hints, refreshed on rebinding.

The session stores the selected heading anchor; unknown anchors reopen the
introduction. Search, selection, menus, and scroll offsets are transient.
Fragment links open headings in the reader. Other repository documents are
not embedded: their references display paths. Search highlights occurrences
in prose, code, and tables, scrolls to the first matching block, and shows a
count and section position. `n`/`N` move between matching sections; Escape
clears the highlights. Queries match displayed text across inline formatting,
not link destinations or Markdown syntax. Documentation shows default keys and updates on
rebuild, not on config reload.

The crate owns no data handle or I/O. The document and text state are prepared
outside render. Search marks use the theme's selection token, refreshed on
theme changes. During search the component renders prepared HTML blocks through
its Markdown reader so block virtualization and scrolling stay intact;
ordinary reading uses the original Markdown. Selection copies plain text,
while `y` copies the original section Markdown. The component owns layout,
selection, and scrolling. Shared tile mechanisms own header/close/stack chrome, menus,
and flip-barrier acknowledgement; visibility and close acknowledge through the
deferred door because the shell calls them during render.

```sh
cargo test -p geode-guide
cargo clippy -p geode-guide --all-targets -- -D warnings
```

See [current features](../../docs/current/features.md#user-guide) for the full
interaction and persistence contract.
